use super::*;

fn checked(arguments: &[&str]) -> Result<Args, String> {
    let args = Args::try_parse_from(arguments).map_err(|_| "Invalid arguments".to_owned())?;
    if let Some(Command::Service(service)) = &args.command {
        service.validate()?;
    }
    Ok(args)
}

#[test]
fn 서비스_입력과_보호된_채널의_충돌은_명확하게_거부한다() {
    // given
    let arguments = [
        vec!["tabularis", "service"],
        vec![
            "tabularis",
            "service",
            "--request",
            "-",
            "--credential-stdin",
        ],
        vec!["tabularis", "service", "--request", "x", "--stdio"],
        vec!["tabularis", "service", "--stdio", "--credential-fd", "7"],
        vec![
            "tabularis",
            "service",
            "--request",
            "x",
            "--credential-fd",
            "2",
        ],
        vec!["tabularis", "--unknown"],
        vec![
            "tabularis",
            "service",
            "approve",
            "id",
            "--decision",
            "edited",
        ],
    ];
    // when
    let actual = arguments.map(|arguments| checked(&arguments).is_err());
    // then
    assert_eq!(actual, [true; 7]);
}
#[test]
fn 단일_요청과_표준입출력과_승인_및_저장_문법은_명시적_값을_유지한다() {
    // given
    let output = std::env::temp_dir().join("owned.geojson");
    let output = output.to_str().unwrap();
    let arguments = [
        vec![
            "tabularis",
            "service",
            "--request",
            "request.json",
            "--write",
            "--credential-stdin",
        ],
        vec![
            "tabularis",
            "service",
            "--stdio",
            "--credential-fd",
            "7",
            "--credential-request-id",
            "request-id",
        ],
        vec![
            "tabularis",
            "service",
            "approve",
            "approval-id",
            "--decision",
            "deny",
            "--reason",
            "reason",
        ],
        vec![
            "tabularis",
            "service",
            "artifact-save",
            "artifact-id",
            "--output",
            output,
        ],
    ];
    // when
    let actual = arguments.map(|arguments| checked(&arguments).unwrap());
    // then
    assert!(
        matches!(&actual[0].command,Some(Command::Service(args)) if args.write&&args.credential_stdin&&args.request.as_deref()==Some("request.json"))
    );
    assert!(
        matches!(&actual[1].command,Some(Command::Service(args)) if args.stdio&&args.credential_fd==Some(7)&&args.credential_request_id.as_deref()==Some("request-id"))
    );
    assert!(
        matches!(&actual[2].command,Some(Command::Service(args)) if matches!(&args.action,Some(ServiceAction::Approve {decision,..}) if decision=="deny"))
    );
    assert!(
        matches!(&actual[3].command,Some(Command::Service(args)) if matches!(&args.action,Some(ServiceAction::ArtifactSave {output,..}) if output.is_absolute()))
    );
}
#[test]
fn 도움말과_버전은_정상_표시하고_기존_플래그는_보존한다() {
    // given
    let arguments = [["tabularis", "--help"], ["tabularis", "--version"]];
    // when
    let actual = arguments.map(|arguments| Args::try_parse_from(arguments).unwrap_err().kind());
    // then
    assert_eq!(
        actual,
        [
            clap::error::ErrorKind::DisplayHelp,
            clap::error::ErrorKind::DisplayVersion
        ]
    );
}

fn argv(arguments: &[&str]) -> Vec<std::ffi::OsString> {
    arguments.iter().map(std::ffi::OsString::from).collect()
}
#[test]
fn 딥링크_위치_인자와_알_수_없는_인자는_일반_실행에서_기본값으로_복구한다() {
    // given
    let arguments = [
        vec!["tabularis", "tabularis-extended://connect?id=1"],
        vec!["tabularis", "--unknown"],
        vec!["tabularis", "-psn_0_12345"],
    ];
    // when
    let actual = arguments.map(|arguments| parse_args(argv(&arguments)).unwrap());
    // then
    assert!(actual
        .iter()
        .all(|args| args.command.is_none() && !args.mcp && !args.debug && args.explain.is_none()));
}
#[test]
fn psn_인자는_무시하고_나머지_플래그는_보존한다() {
    // given
    let arguments = ["tabularis", "-psn_0_12345", "--mcp", "--debug"];
    // when
    let actual = parse_args(argv(&arguments)).unwrap();
    // then
    assert!(actual.mcp && actual.debug && actual.command.is_none());
}
#[test]
fn 일반_실행의_도움말과_버전은_clap에_전달한다() {
    // given
    let arguments = [vec!["tabularis", "--help"], vec!["tabularis", "--version"]];
    // when
    let actual = arguments.map(|arguments| parse_args(argv(&arguments)).unwrap_err().kind());
    // then
    assert_eq!(
        actual,
        [
            clap::error::ErrorKind::DisplayHelp,
            clap::error::ErrorKind::DisplayVersion
        ]
    );
}
#[test]
fn 잘못된_서비스_호출과_mcp_동시_지정은_엄격하게_거부한다() {
    // given
    let arguments = [
        vec!["tabularis", "service"],
        vec!["tabularis", "service", "--unknown"],
        vec!["tabularis", "service", "--request", "x", "--stdio"],
        vec!["tabularis", "--mcp", "service", "--stdio"],
        vec!["tabularis", "--debug", "service", "bogus-action"],
    ];
    // when
    let actual = arguments.map(|arguments| parse_args(argv(&arguments)).is_err());
    // then
    assert_eq!(actual, [true; 5]);
}
#[test]
fn 유효한_서비스_호출은_엄격_경로로_해석한다() {
    // given
    let arguments = ["tabularis", "service", "--request", "request.json"];
    // when
    let actual = parse_args(argv(&arguments)).unwrap();
    // then
    assert!(
        matches!(&actual.command,Some(Command::Service(args)) if args.request.as_deref()==Some("request.json"))
    );
}

#[test]
fn 드라이버_목록_플래그는_psn_인자가_있어도_파싱한다() {
    // given
    let arguments = [
        vec!["tabularis-extended", "--list-drivers"],
        vec!["tabularis-extended", "-psn_0_12345", "--list-drivers"],
        vec!["tabularis-extended", "--list-drivers", "--unknown-extra"],
    ];
    // when
    let actual = arguments.map(|arguments| parse_args(argv(&arguments)).unwrap());
    // then
    assert!(actual[0].list_drivers && actual[1].list_drivers);
    assert!(actual.iter().all(|args| args.command.is_none() && !args.mcp));
    assert!(!actual[2].list_drivers, "an unknown argument falls back to GUI defaults");
}
#[test]
fn 드라이버_목록은_기본_실행과_서비스_모드에서_꺼져_있고_서비스와는_배타적이다() {
    // given
    let default = parse_args(argv(&["tabularis-extended"])).unwrap();
    let service = parse_args(argv(&["tabularis-extended", "service", "--stdio"])).unwrap();
    let mixed = parse_args(argv(&["tabularis-extended", "--list-drivers", "service", "--stdio"]));
    // then
    assert!(!default.list_drivers && !service.list_drivers);
    assert!(mixed.is_err());
}
