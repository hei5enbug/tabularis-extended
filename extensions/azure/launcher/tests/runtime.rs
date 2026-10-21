use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};
use tabularis_azure_launcher::{LaunchError, Layout, REMOVED_ENVIRONMENT};

static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);
const TEST_TIMEOUT: Duration = Duration::from_secs(60);

struct Fixture {
    scratch: PathBuf,
    root: PathBuf,
    executable: PathBuf,
    node: PathBuf,
    entry: PathBuf,
}

struct RunningChild(Child);
impl std::ops::Deref for RunningChild {
    type Target = Child;
    fn deref(&self) -> &Self::Target { &self.0 }
}
impl std::ops::DerefMut for RunningChild {
    fn deref_mut(&mut self) -> &mut Self::Target { &mut self.0 }
}
impl Drop for RunningChild {
    fn drop(&mut self) { let _ = self.0.kill(); let _ = self.0.wait(); }
}

fn fixture_node() -> PathBuf {
    let path = std::env::var_os("TABULARIS_C3B_TEST_NODE").map(PathBuf::from).expect("전용 synthetic Node fixture 경로가 필요합니다");
    assert!(path.is_absolute() && path.is_file(), "검증된 전용 Node fixture의 절대 경로가 필요합니다");
    path
}

impl Fixture {
    fn new(script: &str) -> Self {
        let scratch = std::env::temp_dir().join(format!("tabularis-c3b-공간 fixture-{}-{}", std::process::id(), NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed)));
        let root = scratch.join("한글 bundle with spaces");
        std::fs::create_dir_all(root.join("dist")).unwrap();
        #[cfg(windows)]
        let (executable, node) = (root.join("Cosmos 런처.exe"), root.join("runtime/node.exe"));
        #[cfg(not(windows))]
        let (executable, node) = (root.join("Cosmos 런처"), root.join("runtime/bin/node"));
        std::fs::create_dir_all(node.parent().unwrap()).unwrap();
        std::fs::copy(env!("CARGO_BIN_EXE_tabularis-azure-launcher"), &executable).unwrap();
        std::fs::copy(fixture_node(), &node).unwrap();
        let entry = root.join("dist/driver.mjs");
        std::fs::write(&entry, script).unwrap();
        Self { scratch, root, executable, node, entry }
    }

    fn command(&self) -> Command {
        let mut command = Command::new(&self.executable);
        command.current_dir(&self.scratch).env("PATH", "");
        for name in REMOVED_ENVIRONMENT {
            command.env(name, "synthetic-c3b-env-canary");
        }
        command.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
        command
    }

    fn run(&self, input: Vec<u8>) -> Output {
        communicate(self.command(), input)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.scratch);
    }
}

fn wait(child: &mut Child) -> ExitStatus {
    let deadline = Instant::now() + TEST_TIMEOUT;
    loop {
        if let Some(status) = child.try_wait().unwrap() { return status; }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("synthetic 런처가 제한 시간 안에 종료되지 않았습니다");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn communicate(mut command: Command, input: Vec<u8>) -> Output {
    let mut child = RunningChild(command.spawn().unwrap());
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = child.stdout.take().unwrap();
    let mut stderr = child.stderr.take().unwrap();
    let writer = std::thread::spawn(move || stdin.write_all(&input));
    let reader = std::thread::spawn(move || { let mut bytes = Vec::new(); stdout.read_to_end(&mut bytes).unwrap(); bytes });
    let errors = std::thread::spawn(move || { let mut bytes = Vec::new(); stderr.read_to_end(&mut bytes).unwrap(); bytes });
    let status = wait(&mut child);
    let _ = writer.join().unwrap();
    Output { status, stdout: reader.join().unwrap(), stderr: errors.join().unwrap() }
}

fn error_is_generic(output: &Output, code: &str) -> bool {
    let stderr = String::from_utf8_lossy(&output.stderr);
    !output.status.success() && output.stdout.is_empty() && stderr.starts_with(code)
        && !stderr.contains("synthetic-") && !stderr.contains("fixture") && !stderr.contains("한글 bundle")
        && !stderr.contains("driver.mjs") && !stderr.contains("NODE_")
}

#[test]
fn 번들_노드만_실행하고_고정_인자와_환경_제거_및_작업_폴더를_보존한다() {
    // given
    let fixture = Fixture::new(r#"
import { dirname } from 'node:path';
import { realpathSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
const entry = fileURLToPath(import.meta.url);
const names = ['NODE_OPTIONS','NODE_PATH','NODE_TLS_REJECT_UNAUTHORIZED','NODE_EXTRA_CA_CERTS','NODE_USE_SYSTEM_CA','SSL_CERT_FILE','SSL_CERT_DIR','OPENSSL_CONF'];
const fixed = process.execArgv.length === 1 && process.execArgv[0] === '--max-old-space-size=512' && process.argv.length === 2 && realpathSync.native(process.argv[1]) === realpathSync.native(entry);
console.log([process.version, fixed, names.every(name => !Object.hasOwn(process.env, name)), realpathSync.native(process.cwd()) === realpathSync.native(dirname(dirname(entry)))].join('\n'));
"#);
    // when
    let output = fixture.run(Vec::new());
    // then
    assert!(output.status.success());
    assert_eq!(output.stdout, b"v24.21.0\ntrue\ntrue\ntrue\n");
    assert!(output.stderr.is_empty());
}

#[test]
fn 원본_표준입출력_바이트와_유니코드_줄을_변경하지_않는다() {
    // given
    let fixture = Fixture::new("process.stdin.on('data', chunk => process.stdout.write(chunk)); process.stdin.on('end', () => process.stderr.write('SYNTHETIC STDERR\\n'));");
    let bytes = "{\"jsonrpc\":\"2.0\",\"id\":1}\n  {\"text\":\"한글🚀\"} \n".as_bytes().to_vec();
    // when
    let output = fixture.run(bytes.clone());
    // then
    assert!(output.status.success());
    assert_eq!(output.stdout, bytes);
    assert_eq!(output.stderr, b"SYNTHETIC STDERR\n");
}

#[test]
fn 파이프보다_큰_입력을_역압과_eof까지_손실_없이_전달한다() {
    // given
    let fixture = Fixture::new("process.stdin.on('data', chunk => { if (!process.stdout.write(chunk)) { process.stdin.pause(); process.stdout.once('drain', () => process.stdin.resume()); } }); process.stdin.on('end', () => process.stdout.end());");
    let bytes = format!("{{\"payload\":\"{}\"}}\n", "x".repeat(65_536)).repeat(32).into_bytes();
    // when
    let output = fixture.run(bytes.clone());
    // then
    assert!(output.status.success());
    assert_eq!(output.stdout, bytes);
    assert!(output.stderr.is_empty());
}

#[test]
fn 노드의_종료_코드를_그대로_반환한다() {
    // given
    let fixture = Fixture::new("process.exit(23);");
    // when
    let output = fixture.run(Vec::new());
    // then
    assert_eq!(output.status.code(), Some(23));
    assert!(output.stdout.is_empty());
    assert!(output.stderr.is_empty());
}

#[test]
fn 사용자_인자를_거부하고_값을_오류에_노출하지_않는다() {
    // given
    let fixture = Fixture::new("console.log('ENTRY_RAN');");
    let mut command = fixture.command();
    command.arg("synthetic-argument-secret-canary");
    // when
    let output = communicate(command, Vec::new());
    // then
    assert!(error_is_generic(&output, "INVALID_ARGUMENT:"));
}

#[test]
fn 번들_노드가_없어도_path의_노드로_대체하지_않는다() {
    // given
    let fixture = Fixture::new("console.log('ENTRY_RAN');");
    std::fs::remove_file(&fixture.node).unwrap();
    let mut command = fixture.command();
    command.env("PATH", fixture_node().parent().unwrap());
    // when
    let output = communicate(command, Vec::new());
    // then
    assert!(error_is_generic(&output, "INVALID_BUNDLE:"));
}

#[test]
fn 엔트리가_없으면_경로를_노출하지_않고_거부한다() {
    // given
    let fixture = Fixture::new("console.log('ENTRY_RAN');");
    std::fs::remove_file(&fixture.entry).unwrap();
    // when
    let output = fixture.run(Vec::new());
    // then
    assert!(error_is_generic(&output, "INVALID_BUNDLE:"));
}

#[test]
fn 엔트리_자리에_디렉터리가_있으면_거부한다() {
    // given
    let fixture = Fixture::new("console.log('ENTRY_RAN');");
    std::fs::remove_file(&fixture.entry).unwrap();
    std::fs::create_dir(&fixture.entry).unwrap();
    // when
    let output = fixture.run(Vec::new());
    // then
    assert!(error_is_generic(&output, "INVALID_BUNDLE:"));
}

#[test]
fn 실제_실행_파일의_정규_경로에서_번들_루트를_찾는다() {
    // given
    let fixture = Fixture::new("process.exit(0);");
    // when
    let layout = Layout::from_executable(&fixture.executable);
    // then
    assert_eq!(layout.as_ref().map(|value| &value.root), Ok(&fixture.root.canonicalize().unwrap()));
    assert_eq!(layout.as_ref().map(|value| &value.entry), Ok(&fixture.entry.canonicalize().unwrap()));
}

#[test]
fn 런처_오류_문구에는_임의_경로나_원문_오류가_없다() {
    // given
    let errors = [LaunchError::InvalidArguments, LaunchError::InvalidBundle, LaunchError::InvalidCommandLine, LaunchError::LaunchFailed];
    // when
    let messages: Vec<String> = errors.iter().map(ToString::to_string).collect();
    // then
    assert!(messages.iter().all(|message| !message.contains('/') && !message.contains('\\') && !message.contains("synthetic-")));
}

#[cfg(unix)]
mod unix {
    use super::*;
    use std::io::BufRead;
    use std::os::unix::fs::symlink;
    use std::os::unix::process::ExitStatusExt;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn 실행권한이_있는_텍스트도_쉘로_대체_실행하지_않는다() {
        // given
        let fixture = Fixture::new("console.log('ENTRY_RAN');");
        std::fs::write(&fixture.node, "printf 'SYNTHETIC_SHELL_FALLBACK\\n'\nexit 7\n").unwrap();
        // when
        let output = fixture.run(Vec::new());
        // then
        assert!(error_is_generic(&output, "INVALID_BUNDLE:"));
    }

    #[test]
    fn 쉘_스크립트를_노드_런타임으로_실행하지_않는다() {
        // given
        let fixture = Fixture::new("console.log('ENTRY_RAN');");
        std::fs::write(&fixture.node, "#!/bin/sh\nprintf 'SYNTHETIC_SHELL_FALLBACK\\n'\nexit 7\n").unwrap();
        // when
        let output = fixture.run(Vec::new());
        // then
        assert!(error_is_generic(&output, "INVALID_BUNDLE:"));
    }

    #[test]
    fn 가짜_네이티브_헤더_뒤의_텍스트도_쉘_폴백_없이_실패한다() {
        // given
        let fixture = Fixture::new("console.log('ENTRY_RAN');");
        #[cfg(target_os = "macos")]
        let mut bytes = vec![0xcf, 0xfa, 0xed, 0xfe];
        #[cfg(not(target_os = "macos"))]
        let mut bytes = vec![0x7f, b'E', b'L', b'F'];
        bytes.extend_from_slice(b"\nprintf 'SYNTHETIC_SHELL_FALLBACK\\n'\nexit 7\n");
        std::fs::write(&fixture.node, bytes).unwrap();
        // when
        let output = fixture.run(Vec::new());
        // then
        assert!(error_is_generic(&output, "LAUNCH_FAILED:"));
    }

    #[test]
    fn 네이티브_런타임도_실행권한이_없으면_거부한다() {
        // given
        let fixture = Fixture::new("console.log('ENTRY_RAN');");
        std::fs::set_permissions(&fixture.node, std::fs::Permissions::from_mode(0o644)).unwrap();
        // when
        let output = fixture.run(Vec::new());
        // then
        assert!(error_is_generic(&output, "INVALID_BUNDLE:"));
    }

    #[test]
    fn 번들_밖을_가리키는_엔트리_심볼릭_링크를_거부한다() {
        // given
        let fixture = Fixture::new("console.log('ENTRY_RAN');");
        let outside = fixture.scratch.join("outside.mjs");
        std::fs::rename(&fixture.entry, &outside).unwrap();
        symlink(outside, &fixture.entry).unwrap();
        // when
        let output = fixture.run(Vec::new());
        // then
        assert!(error_is_generic(&output, "INVALID_BUNDLE:"));
    }

    #[test]
    fn 노드의_심볼릭_링크는_번들_안에서도_거부한다() {
        // given
        let fixture = Fixture::new("console.log('ENTRY_RAN');");
        let alternative = fixture.root.join("node-alternative");
        std::fs::rename(&fixture.node, &alternative).unwrap();
        symlink(alternative, &fixture.node).unwrap();
        // when
        let output = fixture.run(Vec::new());
        // then
        assert!(error_is_generic(&output, "INVALID_BUNDLE:"));
    }

    #[test]
    fn 중간_디렉터리의_심볼릭_링크_우회를_거부한다() {
        // given
        let fixture = Fixture::new("console.log('ENTRY_RAN');");
        let outside = fixture.scratch.join("outside-runtime");
        std::fs::rename(fixture.root.join("runtime"), &outside).unwrap();
        symlink(outside, fixture.root.join("runtime")).unwrap();
        // when
        let output = fixture.run(Vec::new());
        // then
        assert!(error_is_generic(&output, "INVALID_BUNDLE:"));
    }

    #[test]
    fn 유닉스_노드는_런처와_동일한_pid로_실행된다() {
        // given
        let fixture = Fixture::new("console.log(process.pid);");
        let scenario = || {
            let mut child = RunningChild(fixture.command().spawn().unwrap());
            let pid = child.id();
            drop(child.stdin.take());
            let mut stdout = child.stdout.take().unwrap();
            let mut bytes = Vec::new();
            let status = wait(&mut child);
            stdout.read_to_end(&mut bytes).unwrap();
            let output = Output { status, stdout: bytes, stderr: Vec::new() };
            (pid, output)
        };
        // when
        let (pid, output) = scenario();
        // then
        assert!(output.status.success());
        assert_eq!(String::from_utf8(output.stdout).unwrap().trim().parse::<u32>(), Ok(pid));
    }

    fn signal_status(signal: i32) -> ExitStatus {
        unsafe extern "C" { fn kill(pid: i32, signal: i32) -> i32; }
        let fixture = Fixture::new("console.log('READY'); process.stdin.resume();");
        let mut child = RunningChild(fixture.command().spawn().unwrap());
        let stdout = child.stdout.take().unwrap();
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut line = String::new();
            std::io::BufReader::new(stdout).read_line(&mut line).unwrap();
            let _ = sender.send(line);
        });
        if receiver.recv_timeout(TEST_TIMEOUT).unwrap() != "READY\n" {
            let _ = child.kill();
            panic!("synthetic Node 준비 신호가 필요합니다");
        }
        if unsafe { kill(child.id() as i32, signal) } != 0 {
            let _ = child.kill();
            panic!("소유 synthetic 프로세스에 신호를 보낼 수 없습니다");
        }
        wait(&mut child)
    }

    #[test]
    fn 유닉스_종료_신호는_노드에_직접_전달된다() {
        // given
        let signal = 15;
        // when
        let status = signal_status(signal);
        // then
        assert_eq!(status.signal(), Some(signal));
    }

    #[test]
    fn 유닉스_인터럽트_신호는_노드에_직접_전달된다() {
        // given
        let signal = 2;
        // when
        let status = signal_status(signal);
        // then
        assert_eq!(status.signal(), Some(signal));
    }
}

#[cfg(windows)]
mod windows {
    use super::*;
    use std::io::BufRead;
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0};
    use windows_sys::Win32::System::Threading::{OpenProcess, WaitForSingleObject, PROCESS_SYNCHRONIZE};

    struct ProcessHandle(HANDLE);
    impl Drop for ProcessHandle {
        fn drop(&mut self) { unsafe { CloseHandle(self.0); } }
    }

    fn tree_fixture(exit: bool) -> Fixture {
        Fixture::new(&format!("import {{ spawn }} from 'node:child_process'; import {{ writeFileSync }} from 'node:fs'; import {{ fileURLToPath }} from 'node:url'; const file=new URL('child.mjs',import.meta.url); writeFileSync(file,'setInterval(() => {{}},1000)'); const child=spawn(process.execPath,['--max-old-space-size=512',fileURLToPath(file)],{{stdio:'ignore'}}); console.log(child.pid); process.stdin.resume(); {};", if exit { "process.stdin.on('end',()=>process.exit(0))" } else { "" }))
    }

    fn tree_stopped(fixture: &Fixture, kill_parent: bool) -> (ExitStatus, bool) {
        let mut child = RunningChild(fixture.command().spawn().unwrap());
        let stdout = child.stdout.take().unwrap();
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut line = String::new();
            std::io::BufReader::new(stdout).read_line(&mut line).unwrap();
            let _ = sender.send(line);
        });
        let line = receiver.recv_timeout(TEST_TIMEOUT).expect("소유 Windows 자손의 준비 신호가 필요합니다");
        let pid = line.trim().parse::<u32>().unwrap();
        let process = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, pid) };
        if process.is_null() { panic!("소유 Windows 자손을 실제로 확인해야 합니다"); }
        let process = ProcessHandle(process);
        if kill_parent { child.kill().unwrap(); } else { drop(child.stdin.take()); }
        let status = wait(&mut child);
        let stopped = unsafe { WaitForSingleObject(process.0, 5_000) } == WAIT_OBJECT_0;
        (status, stopped)
    }

    #[test]
    fn 윈도우_런처가_강제_종료되면_소유_프로세스_트리도_종료된다() {
        // given
        let fixture = tree_fixture(false);
        // when
        let (status, stopped) = tree_stopped(&fixture, true);
        // then
        assert!(!status.success());
        assert!(stopped);
    }

    #[test]
    fn 윈도우_노드가_정상_종료해도_남은_자손은_유지되지_않는다() {
        // given
        let fixture = tree_fixture(true);
        // when
        let (status, stopped) = tree_stopped(&fixture, false);
        // then
        assert!(status.success());
        assert!(stopped);
    }

    #[test]
    fn 윈도우_런타임_디렉터리의_정션_우회를_거부한다() {
        // given
        let fixture = Fixture::new("console.log('ENTRY_RAN');");
        let outside = fixture.scratch.join("outside-runtime");
        std::fs::rename(fixture.root.join("runtime"), &outside).unwrap();
        let setup = fixture.scratch.join("junction-setup.mjs");
        std::fs::write(&setup, "import { symlinkSync } from 'node:fs'; symlinkSync(process.argv[2],process.argv[3],'junction');").unwrap();
        let mut command = Command::new(fixture_node());
        command.arg("--max-old-space-size=512").arg(setup).arg(outside).arg(fixture.root.join("runtime"));
        for name in REMOVED_ENVIRONMENT { command.env_remove(name); }
        let setup_status = command.stdout(Stdio::null()).stderr(Stdio::null()).status().unwrap();
        assert!(setup_status.success(), "소유 Windows 정션 fixture가 필요합니다");
        // when
        let output = fixture.run(Vec::new());
        // then
        assert!(error_is_generic(&output, "INVALID_BUNDLE:"));
    }
}
