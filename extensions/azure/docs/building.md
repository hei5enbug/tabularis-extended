# 독립 checkout에서 빌드하기

이 안내는 Cosmos 저장소 하나에서 빌드하고 현재 OS용 생산 ZIP을 만드는 방법을 설명합니다.

## 고정 SDK 입력

| 입력 | 고정값 |
| --- | --- |
| Node | `24.21.0` |
| pnpm | `10.30.3` |
| Rust | `1.96.0` |
| 공식 Cosmos SDK | `4.10.1` |
| 최소 host runtime | 기본 조회 `0.26.0`, 전체 서비스 `0.26.1-spatial.1` |
| service protocol / 공개 SDK API | `1` / `0.2.0` |

`build-support/sdk`에는 service-contracts와 public plugin-api를 빌드하는 고정 입력 22개가 있습니다.
SDK snapshot에는 root와 각 package의 Apache-2.0 license 파일이 포함됩니다.
`build-support/sdk/provenance.json`은 파일별 크기와 SHA256, 원본 SHA256, upstream 출처를 기록합니다.

- upstream repository: `https://github.com/TabularisDB/tabularis.git`
- upstream base: `b78a40946f072f6b8f2f1a4c80c1e04ff9b54cd4`
- canonical host commit: `a10ba47979320766f0cb706afd48e6cecd7331e8`

schema나 SDK source를 이 snapshot에서 수정하지 마세요.
canonical source 변경을 수용할 때 원본 출처와 파일별 SHA를 함께 갱신하세요.

## 공식 플러그인 계약

[공식 플러그인 가이드](https://github.com/TabularisDB/tabularis/blob/main/plugins/PLUGIN_GUIDE.md)와
[플러그인 시스템 문서](https://tabularis.dev/wiki/plugins)를 기준으로 배포합니다.
ZIP 루트에는 `.tabularium`과 상대 경로의 실행 파일이 있고, 드라이버는 줄 단위 JSON-RPC를 표준입출력으로 처리합니다.
UI는 `__tabularis_plugin__` IIFE로 빌드하며 React·JSX runtime·plugin API는 호스트 제공 전역으로 남깁니다.
UI에서 Tauri API를 직접 호출하지 않습니다.
SDK·Node 런타임은 배포 패키지 안에서 해결하고 개인 설치 경로나 작업 디렉터리를 참조하지 않습니다.
Azure CLI는 해당 인증 방식을 선택할 때만 필요하며 실행 환경의 PATH로 찾습니다.

## bootstrap 실행

고정된 Node와 pnpm을 준비한 뒤 Cosmos checkout에서 실행합니다.

```sh
pnpm bootstrap
pnpm verify:sdk
pnpm typecheck
pnpm --dir ui typecheck
pnpm test:bootstrap
pnpm test:sdk
pnpm test:unit
pnpm test:protocol
pnpm test:ui
```

bootstrap은 SDK 원장을 확인하고 root workspace만 `--frozen-lockfile --ignore-scripts`로 설치합니다.
그 뒤 service-contracts, plugin-api, 드라이버, UI 순서로 현재 checkout 안에서 빌드합니다.
별도 host checkout이나 UI 전용 install은 필요하지 않습니다.

공개 의존성을 받으려면 네트워크 또는 채워진 package cache가 필요합니다.
bootstrap은 lockfile을 변경하지 않습니다.

## 현재 OS용 ZIP 만들기

launcher는 Rust로 빌드합니다.

```sh
cargo build --locked --release --manifest-path launcher/Cargo.toml
```

공식 `nodejs.org/dist/v24.21.0/` archive와
`scripts/package/pins.mjs`의 SHA256이 일치해야 합니다.
아래 경로는 실제 절대 경로로 바꿔주세요.
output 부모 디렉터리는 미리 만들고 기존 output 파일은 사용하지 않습니다.

```sh
node scripts/package/cli.mjs \
  --platform darwin --arch arm64 \
  --source /absolute/tabularis-azure \
  --launcher /absolute/target/release/tabularis-azure-launcher \
  --runtime-archive /absolute/node-v24.21.0-darwin-arm64.tar.gz \
  --output /absolute/output/cosmos-nosql.zip \
  --tar /usr/bin/tar
```

Linux는 `--platform linux`와 native arch를 사용합니다.
ZIP과 tar.gz를 모두 읽는 `/usr/bin/bsdtar`가 필요합니다. Ubuntu에서는 `libarchive-tools`를 설치합니다.
Windows x64는 `--platform win32 --arch x64`, launcher `.exe`, `node-v24.21.0-win-x64.zip`을 사용합니다.
Windows tar는 `%SystemRoot%\System32\tar.exe`의 절대 경로를 전달합니다.
ZIP과 `.sha256` 파일이 발행되며 ZIP 내부 `release.json`에 파일별 크기·SHA와 runtime pin이 있습니다.
앱을 종료하고 검증한 ZIP의 모든 파일을 아래 폴더에 압축 해제합니다. 이전 설치본은 별도 폴더에 백업합니다.

| OS | 설치 폴더 |
| --- | --- |
| macOS | `~/Library/Application Support/tabularis/plugins/drivers/cosmos-nosql/` |
| Linux | `~/.local/share/tabularis/plugins/drivers/cosmos-nosql/` |
| Windows | `%APPDATA%\tabularis\plugins\drivers\cosmos-nosql\` |

앱을 다시 열고 설정의 Plugins에서 `cosmos-nosql`을 활성화합니다.
동봉 launcher와 Node 실행 권한을 보존해야 합니다.
공식 host `0.26.0`부터 기본 읽기 전용 경로를 지원합니다.
공통 서비스가 필요한 기능은 실제 host API 제공 여부를 확인해 활성화합니다.

## 선택 사항인 native CI

`.github/workflows/bootstrap.yml`은 GitHub Actions에서 필요할 때 수동으로 실행합니다.
Actions 성공은 필수 완료 조건이 아닙니다. 실제 OS에서 같은 검증을 직접 실행할 수 있습니다.
Ubuntu 24.04, Windows 2022, macOS 15 Intel에서 각 native x64를 검사합니다.
실행 결과와 확인하지 않은 범위는 [검증 현황](verification.md)에 기록합니다.

CI는 공식 Node archive 다섯 개를 SHA로 검증하고 archive extraction 검사를 모두 수행합니다.
실제 bundled runtime 실행과 생산 ZIP smoke는 해당 runner의 native x64 하나만 수행합니다.
이는 다섯 OS·arch의 runtime을 모두 실행했다는 뜻이 아닙니다.

install 검사에는 다음 절대 경로를 전달합니다.

| 환경 변수 | 값 |
| --- | --- |
| `TABULARIS_C3B_TEST_LAUNCHER` | 현재 OS의 release launcher |
| `TABULARIS_C3B_TEST_NODE_ARCHIVE` | 현재 OS·arch의 공식 archive |
| `TABULARIS_C3B_TEST_PYTHON` | 실제 Python executable |
| `TABULARIS_C3B_TEST_NODE_CACHE` | 다섯 검증 archive가 있는 전용 cache |

CI는 ZIP·checksum·release 원장·production 보고서를 artifact로 업로드합니다.
설치 smoke의 임시 extraction은 native host installer 실행과 구분합니다.
