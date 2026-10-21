//! Process fixture standing in for Azure CLI. It never contacts Azure; the
//! script prints a prepared response and records how it was invoked.

#![cfg_attr(not(unix), allow(dead_code))]

use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use serde_json::{json, Value};

pub const TENANT: &str = "00000000-0000-0000-0000-0000000000a1";
pub const OBJECT: &str = "00000000-0000-0000-0000-0000000000b2";
pub const USER: &str = "user@example.com";
pub const HOST: &str = "synthetic.database.windows.net";

pub fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

pub fn token(claims: Value) -> String {
    let payload = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims).unwrap());
    format!("eyJhbGciOiJub25lIn0.{payload}.synthetic-signature")
}

pub fn claims_with(tenant: &str, object: &str, user: &str, exp: u64) -> Value {
    json!({
        "tid": tenant,
        "oid": object,
        "upn": user,
        "aud": "https://database.windows.net/",
        "exp": exp
    })
}

pub fn valid_token() -> String {
    token(claims_with(TENANT, OBJECT, USER, unix_now() + 3_600))
}

pub struct FakeCli {
    dir: PathBuf,
}

impl FakeCli {
    /// Script body runs after the invocation has been recorded.
    pub fn script(body: &str) -> Self {
        let dir =
            std::env::temp_dir().join(format!("sqlserver-plus-fake-az-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("az");
        let script = format!(
            "#!/bin/sh\nif [ \"$1\" = \"--probe\" ]; then exit 0; fi\nd=\"$(dirname \"$0\")\"\nprintf 'x\\n' >> \"$d/calls.txt\"\nprintf '%s\\n' \"$@\" > \"$d/args.txt\"\n{body}\n"
        );
        std::fs::write(&path, script).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let cli = Self { dir };
        cli.wait_until_executable();
        cli
    }

    /// Prints `{"accessToken": ...}` for the given token.
    pub fn responding_with(token: &str) -> Self {
        let cli = Self::script("cat \"$d/response.json\"");
        std::fs::write(
            cli.dir.join("response.json"),
            serde_json::to_vec(&json!({"accessToken": token, "tokenType": "Bearer"})).unwrap(),
        )
        .unwrap();
        cli
    }

    pub fn path(&self) -> String {
        self.dir.join("az").to_string_lossy().into_owned()
    }

    pub fn args(&self) -> Vec<String> {
        std::fs::read_to_string(self.dir.join("args.txt"))
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    pub fn invocations(&self) -> usize {
        std::fs::read_to_string(self.dir.join("calls.txt"))
            .map(|calls| calls.lines().count())
            .unwrap_or(0)
    }

    pub fn file(&self, name: &str) -> PathBuf {
        self.dir.join(name)
    }

    // A freshly written script can fail to exec with ETXTBSY while another
    // test thread's fork still holds the write descriptor.
    fn wait_until_executable(&self) {
        for _ in 0..200 {
            match std::process::Command::new(self.path())
                .arg("--probe")
                .output()
            {
                Ok(_) => return,
                Err(_) => std::thread::sleep(Duration::from_millis(10)),
            }
        }
        panic!("fake Azure CLI never became executable");
    }
}

impl Drop for FakeCli {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}
