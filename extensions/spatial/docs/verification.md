# 검증 현황과 재현

공통 서비스와 실제 PostGIS·지도 UI·GUI/MCP/CLI 통합 검증을 마쳤다.
Windows 라이브러리 검사 실행 파일의 manifest를 보정한 뒤 실제 설치 검사도 통과했다.
실제 Azure의 최소 읽기는 확인했지만 SQL 플러그인의 읽기 전용 권한 확인과 전체 수용 검사는 미완료다.
아래의 native는 해당 OS에서 실제 실행했다는 뜻이며, synthetic은 대체 서버·드라이버 또는 플랫폼 입력을 사용했다는 뜻이다.

## 확인한 동작

| 영역 | 실행 환경과 결과 | 증거의 범위 |
|---|---|---|
| 공간 모델 | Spatial Rust core 68개 통과 | 원본 식별·좌표/행/바이트 제한·지도 버전 |
| 기존 PostgreSQL 드라이버 | unit 402개, 실제 PostGIS 74개 통과 | 원본 EWKB·geometry/geography·SRID·Z/M·NULL/EMPTY·읽기 전용·취소 |
| 공통 서비스 공간 조회 | 실제 PG17/18 조회 4개, PG18 대용량 export 1개 통과 | 불변 쿼리 결과·원 SQL 재실행 0회·변환·페이지·연결별 취소·32 MiB 초과 명시 export |
| 지도 UI | React 검사 97개, typecheck/build 통과 | 레이어·속성·선택·상태·오래된 응답 차단·artifact 저장 호출 |
| WebView 렌더링 | macOS WKWebView 22개·열기/닫기 두 번 통과 | 실제 WebGL·로컬 CSS/worker·모달 열기/닫기. 서비스는 mock이며 실제 Tauri 통합 증거와 구분 |
| 대용량 지도 | 실제 WKWebView에서 10,000 feature·250,000 coordinates 통과 | 최대 rAF 간격 50 ms, 100 ms 초과 0회, worker·listener·asset 회수 확인 |
| 실제 앱 통합 | 실제 CLI/MCP parity 1개와 Tauri GUI 통합 1개 통과 | 원본 결과·페이지·export 비교, 별도 프로세스 지도 적용 ACK·선택·버전 충돌·닫기/재열기 |
| 공통 transport | 전체 서비스 검사 204개 통과, 기존 자식 프로세스 검사 1개 별도 실행 | operation 스키마·정책·결과·오류·CLI/MCP registry·실제 로컬 IPC |
| 네이티브 진입점 | native 16개·지도 ACK 1개·산출물 취소 1개·CLI 3개·MCP 7개·기존 회귀 23개, host check 통과 | Tauri 명령 권한·별도 native core의 IPC·정확한 SQL·비밀 입력·bounded 출력. 실제 앱 화면 검사는 별도 |
| 쿼리 화면 | 격리 검사 32개, Notebook 16개, frontend build/typecheck/ESLint 통과 | 고정된 연결·정확한 SQL·결과 참조·세션·취소·명시적 쓰기 승인 |
| 기존 SQL Server 드라이버 | unit 244개·conformance 3개·synthetic Azure 33개·clippy 통과 | Entra 사용자/앱 인증 경로·토큰 갱신·TLS 검증·원본 SQL·CRUD. 실제 CLI 토큰 획득 확인, 데이터 연결 수용 검사는 미완료 |
| Cosmos | unit 625개·protocol 14개·synthetic live harness 74개 통과 | 공식 SDK의 문서/페이지/RU/429/ETag 계약, 실제 CLI 토큰 획득 확인, 데이터 연결 수용 검사는 미완료 |
| 실제 설치 | macOS ARM64와 Rosetta x64 Cosmos ZIP, Spatial ZIP을 수정 호스트 설치기로 검사 | ZIP 모든 파일의 SHA·권한·asset MIME, 동봉 Node 실행, initialize/shutdown, 빈 PATH |
| OS 공통 설치 검사 | macOS ARM·Intel와 Linux x64 실제 호스트 설치 통과 | Windows도 검사 manifest 보정 후 실제 설치 통과 |
| 소스 재현 | 공개 upstream 3개를 고정 SHA로 fetch하고 patch 적용 후 Git tree 검증 통과 | 로컬 비공개 호스트 레포를 내려받을 필요 없음 |
| 독립 디렉터리 빌드 | 한글·공백 경로에서 frozen install, 공통 계약/API, UI build/test, ZIP 생성 통과 | 기존 작업 폴더의 node_modules나 dist를 복사하지 않음 |

각 행은 해당 변경 시점의 검사다. 서로 다른 실행의 통과 수를 합산해 최종 단일 실행 결과로 표시하지 않는다. 중간 실패는 삭제하지 않았으며 수정 뒤 필요한 범위를 다시 검사했다. 호스트 전체 strict clippy는 기존 코드의 경고 때문에 실패한다. 변경 영역의 새 경고와 기존 경고를 구분해 검사했으며 전체 clippy 통과라고 주장하지 않는다.

초기 독립 빌드 ZIP은 488,269 bytes·33 files이며 SHA-256은
`df9032d9c3f4ed4b87197ae1b4365bbc1598de6e1a9a2e7620cbb03a37ff9d65`다.
증분 렌더링 수정 후 실제 앱에 적용한 최종 ZIP은 488,669 bytes·33 files이며 SHA-256은
`04db4b3480a865e515c4dbaa2b12c0e0c1f98c34043be5eb6e74d67aca166f85`다.

## 실제 fixture

| 환경 | 고정 버전 | 이미지 digest |
|---|---|---|
| PG17 | PostgreSQL 17.7 / PostGIS 3.5.2, ARM64 | `sha256:bac8128cd62a35471a1e97966861aa2ac88c8d2f408767a425ae70695a607c99` |
| PG18 | PostgreSQL 18.6 / PostGIS 3.6.4, amd64 emulation | `sha256:7e00e8c3539fdd43f513b98806c8204714dcd09dea683c259e333d7690317119` |
| 로컬 SQL Server | SQL Server 2022 Developer 16.0.4245.2, amd64 emulation | `sha256:49b45a911dc535e9345fbfd7101a1bd8a1e190a5f29b877ef75387a061e5fcf0` |
| 실제 WebView | macOS 26.5 (25F71), WebKit 21624.2.5.11.4 | React 19.2.4, MapLibre 6.11.2 |

emulation 결과로 native 성능을 추정하지 않는다. 로컬 fixture는 검사용 임시 스키마만 생성·정리했고 기존 사용자 DB나 Azure 자원을 사용하지 않았다.

## 재실행

먼저 [빌드 절차](building.md)를 수행한다. 일반 검사는 Azure 자격 증명을 요구하지 않는다.

```sh
pnpm test:sdk
node --test tests/bootstrap.test.mjs
cargo test --locked --workspace
pnpm test:ui
node --test tests/install/*.test.mjs
cargo test --locked --manifest-path ../tabularis-app-source/src-tauri/Cargo.toml --lib services::
```

호스트의 실제 PostGIS 검사는 `TABULARIS_TEST_POSTGIS_PLUGIN`에 빌드한 기존 PostgreSQL 플러그인의 절대 경로를, `TABULARIS_TEST_POSTGIS17_PORT`와 `TABULARIS_TEST_POSTGIS18_PORT`에 전용 로컬 fixture 포트를 지정한다. DB는 `spatial_fixture`, 사용자는 `postgres`, 접속 주소는 `127.0.0.1`이다. 두 fixture 모두 PostGIS 확장이 필요하다. 실제 검사는 임시 스키마를 생성하므로 전용 테스트 DB에서 실행한다.

```sh
cargo test --locked --manifest-path ../tabularis-app-source/src-tauri/Cargo.toml \
  --lib service_spatial::tests::live -- --ignored --test-threads=1
cargo test --locked --manifest-path ../tabularis-app-source/src-tauri/Cargo.toml \
  --lib service_spatial::tests::export_live -- --ignored --test-threads=1
```

명시 실행에서 입력이 없으면 실패한다. 일반 unit 실행의 `ignored`는 실제 DB 검증 통과를 뜻하지 않는다. 계획의 JavaScript service fixture 예제에 해당하는 정책·dispatch·공간 결과 검사는 Rust `services`와 `service_spatial::tests`에서 실제 서비스와 mock DB 경계로 구현했다. 별도 JavaScript mock 서비스로 제품 동작을 복제하지 않는다.

## 아직 구분해야 하는 완료 조건

- 최소 읽기 확인과 전체 수용 검사를 구분한다. SQL의 읽기 전용 주체 확인, MFA/앱 인증·갱신·CRUD와
  Cosmos 교차 파티션 정렬·집계·페이지 재개는 미완료다.
- GitHub Actions는 수동 실행하는 선택 사항이다. 실제 OS에서 직접 실행한 검증도 환경과 결과를 기록해 근거로 사용한다. workflow 작성이나 synthetic 검사를 native 실행으로 간주하지 않는다.

최종 코드 `6d05c3c`의 [CI 실행 37193323425](https://github.com/hei5enbug/tabularis-spatial/actions/runs/37193323425)에서
Linux·Mac Intel 설치와 기존 드라이버 검사는 통과했다.
Windows 2022는 OpenSSL 및 호스트 컴파일까지 성공했지만 검사 실행 파일 시작 시
`0xc0000139 (STATUS_ENTRYPOINT_NOT_FOUND)`로 종료됐다.
실패한 DLL·심볼은 이 로그만으로 특정할 수 없으며 Windows 설치 통과로 간주하지 않는다.
의존성 코드를 확인한 결과 `rfd`는 Common Controls v6를 요구하지만 Tauri의 기본 리소스는
일반 앱 실행 파일에만 연결됐다. 검사 실행 파일에 같은 manifest를 넣도록 CI를 보정했다.
수정 `e34fdda`의 [Windows 단독 실행 37195121887](https://github.com/hei5enbug/tabularis-spatial/actions/runs/37195121887)에서
설치 검사가 통과했다. Mac·Linux·드라이버 검사는 반복하지 않았다.

정확한 배포 소스는 [upstream manifest](../integration/upstreams.json)의 원본 SHA·수정 소스 SHA·Git tree·패치 SHA-256으로 고정한다. CI 로그와 배포 ZIP의 파일별 checksum을 함께 보관한다.

## 최종 통합 실행 기록

2026-10-04 macOS ARM64에서 사용자 프로필과 분리한 실제 실행 파일을 사용했다.
GUI 식별자는 `dev.tabularis.spatial-verification-01a1000d`이며 실제 Azure에는 접속하지 않았다.

| 검사 | 관찰 결과 | 검증 자료 |
|---|---|---|
| CLI/MCP parity | 실제 PostGIS 쿼리·페이지·공간 변환·artifact 파일 내용 동일, 쓰기 거부 | 격리 실행 보고서 |
| 실제 GUI·CLI·MCP | 시작 직후 첫 지도 요청과 이후 조작 모두 적용 완료 ACK, 충돌 시 기존 상태 유지 | 격리 실행 보고서 |
| 대용량 WKWebView | 전체 1만 객체·25만 좌표, 목록 100개/페이지, 마지막 객체 접근, 100 ms 초과 0회 | 성능 fixture 결과 |
| 관련 회귀 | UI 47개, 데이터 제한 11개, 호스트 서비스 훅 17개 통과 | UI·데이터·서비스 훅 검사 로그 |

마지막 GUI 검사에 사용한 호스트 소스는 `1ba68de491db06c2d82a18e12107c34fd82341ec`다.
실행 파일 SHA-256은 `c33663234fd9e4e588f051161ffd6d22ea2940e763c98dd3eb07343327d014fd`다.
전체 결과의 반복 전송을 새 페이지의 증분 갱신으로 바꿨다. 성능 fixture의 크기와 100 ms 기준은 유지했다.
최초 성능 실패와 시작 직후 GUI 요청 유실도 원본 로그에 보존했다.

실제 앱 검사는 `TABULARIS_TEST_HOST_BINARY`, `TABULARIS_TEST_POSTGIS_PLUGIN`,
`TABULARIS_TEST_POSTGIS_MANIFEST`, `TABULARIS_TEST_POSTGIS17_PORT`, `TABULARIS_TEST_SPATIAL_ZIP`을
전용 로컬 fixture의 절대 경로·포트로 지정한 뒤 호스트에서 실행한다. ZIP 옆의 `.sha256` 파일도 필요하다.

```sh
pnpm test:service:parity
pnpm test:spatial:webview
```

수정한 호스트를 위의 격리 식별자로 빌드해야 한다. 기존 사용자 앱을 검사용으로 종료하거나 조작하지 않는다.
WKWebView 성능 검사는 Spatial 저장소의
`node tests/integration/wkwebview/run.mjs --performance`로 실행한다.
이 검사는 실제 MapLibre·WebGL을 사용하지만 서비스는 mock이므로 실제 앱 검사와 구분한다.

| 계획 요구 사항 | 구현·검증 상태 |
|---|---|
| R01–R04 | 기존 PostgreSQL 드라이버 재사용과 실제 PG17/18 공간·SRID·쿼리/테이블 조회 검증 완료 |
| R05–R08 | 지도 UI·대용량·취소·transport 동등성·프로세스 간 지도 적용 검증 완료 |
| R09–R10 | 읽기 전용·비밀 보호·연결/session 격리·지도 버전 검증 완료 |
| R11–R12 | Azure SQL·Cosmos 코드와 synthetic 검사 완료. 실제 최소 읽기 확인, SQL 읽기 전용 권한 확인·쓰기·갱신·전체 parity는 미완료 |
| R13 | 패키지·호스트 패치·독립 빌드 완료. Mac·Linux·Windows 실제 설치 통과 |
| R14 | 위 실행 근거에 따름. 보류·미완료 항목을 전체 통과로 합산하지 않음 |


## Azure CLI 어댑터 확인

2026-10-04에 로그인된 포털과 같은 사용자로 Azure CLI 로그인과 리소스 설정 조회를 확인했다.
앱의 Rust 어댑터로 SQL·Cosmos 토큰 획득을 실제로 실행했고 두 대상 모두 통과했다.
Cosmos 토큰의 대상 값은 공개 리소스 ID로 반환되어 Cosmos 연결에만 해당 값을 허용했다.
SQL에는 이를 허용하지 않는다. 원본 토큰, 사용자·테넌트·구독 식별자와 실제 리소스 이름은 이 기록에 넣지 않았다.

인증 어댑터의 경계·계정 변경·시간 초과·출력 상한 검사와 기존 OAuth 회귀가 통과했다.
Cosmos 연결 단위 검사, 두 드라이버의 CLI 소스 UI 검사, TypeScript 검사와 UI 빌드도 통과했다.

실제 SQL 테스트 DB에 대한 읽기 전용 연결은 `AUTH_REQUIRED`로 실패했다.
실제 Cosmos 개발 계정에서 공식 SDK의 데이터베이스 메타데이터 조회는 HTTP 403으로 실패했다.
해당 사용자에게 직접 배정된 Cosmos 데이터 역할은 없었고 계정에는 IP 허용 목록이 있었다.
후속 응답에서는 IP 방화벽에 따른 `network_access` 거부를 확인했다.
그룹을 통한 데이터 역할 적용 여부는 확인하지 못했으므로 읽기 권한이 없다고 단정하지 않는다.
인증과 네트워크 허용 확인이 남아 있다. 역할이나 방화벽은 변경하지 않았다.
DB나 문서의 생성·수정·삭제는 실행하지 않았다. 전체 Azure 연동 수용 검사는 완료 상태가 아니다.

Windows 설치의 이전 보류 항목은
[단독 실행 37195121887](https://github.com/hei5enbug/tabularis-spatial/actions/runs/37195121887)의 성공으로 종료했다.
이 결과는 Azure CLI 어댑터 추가 전 호스트에 대한 설치 검사이며 새 인증 코드는 해당 CI에서 실행하지 않았다.

## 공식 0.26.0 호환 경로와 문서 대조

2026-10-06 기본 호환 경로를 추가했다. 기존 전체 서비스 검증 기록은 수정 호스트에 대한 기록이다.
이번 지도 UI build/typecheck와 111개 검사, 패키지 검사 29개, 수정 호스트의 실제 지도 ZIP 설치 검사 1개가 통과했다.
공식 macOS ARM64 Tabularis 0.26.0 프로필에 UI 전용 패키지를 설치했다.
이 최초 설치 검사에는 공식 앱의 실제 DB 지도 조회와 Azure 데이터 연결을 포함하지 않았다.

설치 ZIP은 648,095 bytes·33 files이며 SHA256은
`75b41bc70c56eafa87611a64126aec103dfa764636b853476eb40aacef71f108`다.
패키지 내부 원장과 모든 파일의 해시를 대조했다.

[Building Plugins](https://tabularis.dev/wiki/building-plugins),
[Plugin Guide](https://github.com/TabularisDB/tabularis/blob/main/plugins/PLUGIN_GUIDE.md),
[Plugin Tutorial](https://github.com/TabularisDB/tabularis/blob/main/plugins/PLUGIN_TUTORIAL.md),
[SQL Server 플러그인](https://github.com/TabularisDB/tabularis-sqlserver-plugin)의 README와 관련 문서를 대조했다.
UI 전용 manifest, 최소 버전, 공개 UI API와 슬롯, 연결 metadata, stdio와 취소 규약을 확인했다.
문서 예시와 RPC 입력이 다른 부분은 고정한 0.26.0 소스의 실제 계약을 따른다.
공식 앱에는 공유 서비스와 asset API가 없으므로 제한형 기본 지도를 선택하며, 전체 MCP·CLI 지도 기능은 수정 호스트에 남긴다.

후속 상한 보정에서 호스트의 truncated 응답을 보존하고 GeoJSON 닫는 바이트도 계산했다. 관련 10개 검사와 build/typecheck가 통과했다.
공식 앱의 Plugin Center에서 두 플러그인 활성화를 확인했다.

## 공식 앱의 실제 지도 조회와 후속 설치

공식 0.26.0의 테이블 툴바는 빈 UI context를 전달한다. 이때 활성 PostgreSQL 연결을 고정하고
사용자가 스키마·테이블을 입력해 기존 기본 지도 경로를 열도록 수정했다.
명시적으로 전달된 다른 드라이버나 연결 정보는 활성 연결로 덮어쓰지 않는다.
관련 UI 검사 7개와 typecheck·build가 통과했다.

후속 ZIP은 648,400 bytes·33 files이며 SHA256은
`80d9d2b557be405a9b8ecd91f0528fd5f9a224d36158d87b34af9561773d2681`이다.
내부 원장의 모든 파일 해시를 확인하고 실제 공식 macOS ARM64 앱에 다시 설치했다.

로컬 PostgreSQL 17.7·PostGIS 3.5.2에 작업 전용 테이블을 만들고 공식 앱에서 조회했다.
테이블 툴바의 Map 버튼과 테이블 입력 모달을 확인했고, SRID 4326의 점·선·면이
실제 WebGL 지도에 3 features·8 coordinates로 표시됐다.
점을 선택했을 때 `id: 1`과 한글 속성 `label: "서울 점"`도 확인했다.
검사 후 작업 전용 연결과 테이블을 제거했다. 실제 Azure 데이터 조회는 반복하지 않았다.

[원격 실행 37370384862](https://github.com/hei5enbug/tabularis-spatial/actions/runs/37370384862)의
실패 job은 계정 결제·사용 한도 때문에 시작되지 않았다. 이 실패를 코드 검사 실패로 기록하지 않는다.

커밋 대상의 추적 파일과 신규 파일을 Gitleaks 8.30.1로 검사했다.
탐지 3건은 합성 페이지 토큰과 PostgreSQL patch의 합성 비밀번호 fixture이며 실제 비밀 값이 아님을 확인했다.
Git 이력의 탐지 4건도 같은 합성 fixture였다. 패키지·검사 보고서·실제 인증정보는 Git에 추가하지 않는다.

## 공개 전 보안 검사와 최신 공식 가이드 대조

2026-10-06 공개 대상 Git 파일과 전체 Git 이력을 다시 검사했다.
원격에는 main 한 개만 있으며 태그·issue·PR·release는 없었다.
추적된 환경 파일·개인 키·자격 증명 파일은 없고, Gitleaks의 이력 탐지 4건은 위의 합성 fixture였다.
GitHub Actions 산출물 16개와 다운로드 가능한 실행 로그의 텍스트 및 ZIP 안의 텍스트도 검사했다.
공개 콘텐츠에서 실제 비밀 값을 발견하지 않았다. 원본 검사 보고서는 저장소 밖에 보관한다.

최신 공식 문서는 Tabularis main
[`c0fe758325e955d5f364bf3150ea0822c6591469`](https://github.com/TabularisDB/tabularis/tree/c0fe758325e955d5f364bf3150ea0822c6591469) 기준이다.
[Building Plugins](https://tabularis.dev/wiki/building-plugins),
[Plugin Guide](https://github.com/TabularisDB/tabularis/blob/c0fe758325e955d5f364bf3150ea0822c6591469/plugins/PLUGIN_GUIDE.md),
연결 metadata 문서·튜토리얼과 공식 SQL Server 플러그인 README를 대조했다.

| 확인 항목 | 결과 |
|---|---|
| 패키지·최소 버전 | 패키저가 `.tabularium`을 생성하고 `plugins/drivers/spatial/`에 설치한다. 기본 최소 버전은 0.26.0이며 실제 공식 앱 설치를 확인했다. |
| UI 번들 | IIFE 전역 `__tabularis_plugin__`과 default export를 사용한다. React·JSX·plugin API는 외부화하고 공식 전역을 사용한다. 같은 module을 여러 슬롯에서 사용하는 것은 가이드가 허용한다. |
| UI 보안 | Tauri를 직접 import하거나 호출하지 않는다. 호스트 동작은 plugin API를 사용하고 지도 DOM은 해당 컴포넌트 안에서 관리한다. |
| 기본 기능과 확장 기능 | 공식 슬롯 `data-grid.toolbar.actions`에서 기본 지도를 연다. `app.map.renderer`와 `service_protocol`은 수정 호스트 전용이며 서비스·asset API가 있을 때만 전체 기능을 사용한다. |

Spatial은 기존 PostgreSQL 드라이버를 사용하는 UI 전용 패키지이며 DB 실행 파일을 추가하지 않는다.
로케일 파일과 `defineSlot`은 선택 사항이다. 현재의 legacy slot props 형식은 가이드에서 계속 지원한다.
live 레지스트리 스키마 조회는 HTTP 403으로 실패했으므로 UI 전용 매니페스트와 확장 필드의
레지스트리 수용 여부는 확인하지 못했다. 공식 앱의 직접 설치 결과와 레지스트리 승인을 구분한다.

이번 수정은 README·검증 기록과 Actions 실행 조건에 한정했다.
워크플로는 `workflow_dispatch`만 사용하고 `contents: read`를 유지한다.
YAML을 파싱해 수동 실행 조건을 확인했다. 제품 코드와 기존 검사 입력이 바뀌지 않아 전체 테스트는 반복하지 않았다.

## Azure SQL의 읽기 전용 후속 검증

2026-10-06 commit `4ba5b7b`에서 후속 검증을 시작했다.
기존 CLI 로그인으로 SQL scope 토큰과 관리 메타데이터를 메모리에서 처리했다.
SQL 테스트 DB 두 곳에 pyodbc 5.3.0·Microsoft ODBC Driver 18로 접속했다.
`Encrypt=yes`, `TrustServerCertificate=no`, `ApplicationIntent=ReadOnly`를 사용했다.
두 DB 모두 `SELECT 1`이 1을 반환했다. 사용자 테이블 내용과 원본 오류는 출력하지 않았다.
현재 요청에서 이전 인증·방화벽 실패는 재현되지 않았다. ODBC 성공을 SQL 플러그인의 성공으로 대신하지 않는다.

고정 SQL Server upstream에 기존 patch를 적용하고 tree `cdec5c7b16870651204e2537f7a684aa6b9d996b`를 확인했다.
Rust 1.96.0의 `cargo build --locked --bin sqlserver-plugin`으로 빌드했다.
`verify-full`, `entra_user`, matching transient 토큰과 `read_only: true`로 `service_test`를 실행했다.
`startup_script`와 쓰기 RPC는 사용하지 않았다. 응답을 받을 때까지 stdin을 열어 두었다.
최초 probe는 stdin을 일찍 닫아 `CANCELLED`가 됐으며, 이 결과를 Azure 오류로 분류하지 않는다.

| 경로 | 관찰 결과 | 확인한 원인과 한계 |
|---|---|---|
| ODBC의 테스트 DB 두 곳 | `SELECT 1` 성공 | 현재 로그인·네트워크·TLS를 확인. DB 주체가 읽기 전용이라는 증거는 아님 |
| SQL 플러그인의 첫 테스트 DB | `CAPABILITY_UNAVAILABLE` | 권한 조사 결과가 `readonly::probe`의 10,000행 상한을 초과 |
| SQL 플러그인의 다른 테스트 DB | `WRITE_NOT_ALLOWED` | DB 주체를 읽기 전용으로 입증할 수 없음 |
| 동일 권한 probe의 ODBC 조회 | `INSERT`·`UPDATE`·`DELETE`와 DDL 권한 확인 | 두 DB 모두 현재 주체가 드라이버의 읽기 전용 요구를 충족하지 않음 |

첫 DB의 내부 원인은 저장소 밖 진단 복사본에서 안전한 오류 코드만 관찰해 `RESOURCE_LIMIT`로 확인했다.
같은 권한 조회의 행 수를 별도 COUNT 쿼리로 확인해 10,000행 초과를 검증했다.
원본 오류·토큰·실제 리소스 이름은 출력하거나 Git에 남기지 않았다.
권한·역할·방화벽·리소스를 변경하지 않았고 검사 상한과 읽기 전용 보호도 완화하지 않았다.
읽기 전용 실행을 수용하려면 별도로 준비된 제한 주체와 확인 가능한 권한 구성이 필요하다.
서비스 주체 인증·앱 내 사용자 인증·갱신·CRUD·전체 GUI/CLI/MCP 비교는 미완료다.

실제 Cosmos 플러그인의 탐색·최소 읽기는
[Azure 후속 기록](https://github.com/hei5enbug/tabularis-azure/blob/main/docs/verification.md#기존-cli-로그인의-읽기-전용-후속-검증)에 있다.

## 외부 배경지도 동의와 후속 보안 검사

저장된 지도와 CLI/MCP의 지도 상태에 있는 URL은 이전 렌더러에서 UI 확인 없이 로드될 수 있었다.
`MapEngine.apply`는 현재 모달에서 승인한 URL만 MapLibre에 전달하도록 보완했다.
모달은 동의 전에도 공간 레이어를 단색 배경으로 표시한다. 승인한 URL과 다른 URL·새 창에서는 재확인한다.
동의 값은 지도 상태나 저장 파일에 넣지 않는다. 확인란을 해제하면 단색 배경으로 돌아간다.
동의 전의 제한된 표시는 완전한 화면 적용 ACK로 표시하지 않는다.
회귀 검사는 동의 전 차단·해당 URL 적용·다른 URL 차단·동의 철회를 확인한다.
기존 source 교체 검사도 승인된 외부 스타일로 실행해 같은 동작을 확인했다.

root/UI 워크스페이스의 최초 `pnpm audit --json`은 공개 취약점 항목 8개를 보고했다.
Vite 7.3.6·Vitest 4.1.11로 갱신하고 Vite의 esbuild를 지원 범위 안의 0.28.1로 고정했다.
최종 audit는 exit 0, 모든 심각도에서 0건이다. UI 119개와 typecheck를 포함한 UI 빌드가 통과했다.
고정 upstream patch와 MapLibre 6.11.2는 변경하지 않았다.

Gitleaks 8.30.1의 현재 추적 파일 검사에서 탐지한 3건은 기존 합성 fixture와 같은 경로·행이었다.
해당 파일은 이번 작업에서 바뀌지 않았다. 실제 비밀 값 탐지는 없었다.
기존 전체 Git 이력·공개 Actions 로그·산출물 검사 근거는 보존했다.

공식 Tabularis main은 위의 `c0fe758325e955d5f364bf3150ea0822c6591469`와 같았다.
최신 Plugin Guide·Building Plugins·연결 metadata 계약을 다시 확인했다.
UI 전역·외부화·공식 기본 슬롯과 수정 호스트 전용 확장을 구분하는 계약은 유지한다.
live 레지스트리 스키마 GET은 다시 HTTP 403으로 실패했다. 레지스트리 수용 여부는 미확인이다.
새 ZIP의 실제 설치·OS별 실행과 이전 PostGIS 통합 검사는 이번에 반복하지 않았다.

## macOS 로컬 앱에 최신 패키지 반영

2026-10-06 사용자 요청으로 실제 설치된 공식 앱을 0.24.0에서 0.26.0으로 갱신했다.
공식 ARM64 앱 archive의 GitHub 배포 SHA256, 코드 서명과 Apple 공증을 확인했다.
이전 앱과 설정 파일은 앱 프로필과 별도의 private 백업 디렉터리에 보관했다.
설치 직후 기존 설정 파일의 바이트가 유지되는 것을 확인했다.

commit `4aa07c2`의 UI 빌드로 Spatial ZIP을 만들었다.
ZIP은 648,547 bytes·33 files이며 SHA256은
`23fcb715abecc328623f35db109958866f6d7ae3f6f2a38c2801a1183d2d6df4`다.
ZIP과 설치 폴더의 모든 파일을 `release.json` 원장으로 검증했다.
공식 앱의 `plugins/drivers/spatial/`에 설치하고 UI 전용 플러그인의 native 등록을 확인했다.
새 GUI 창과 함께 설치한 Cosmos 런타임의 실행도 확인했다.
앱의 CLI 실행 파일도 0.26.0을 반환한다.

새 공식 MCP 프로세스의 initialize·tools/list는 성공했고 플러그인 로딩 오류는 없었다.
화면 자동 조작은 macOS 접근성 권한 거부 `-25211`로 실행하지 못했다.
이번 설치에서 실제 지도 조작·DB 조회와 다른 OS 실행을 추가로 수행하지 않았다.
이미 실행 중인 이전 MCP 프로세스에는 클라이언트의 재연결이 필요하다.
# 저장소 내부 SDK 전환 최종 검증 — 2026-10-07

macOS ARM64에서 Node `24.21.0`과 pnpm `10.30.3`으로 검사했습니다.
형제 호스트, 기존 `node_modules`, SDK `dist`가 없는 한글·공백 경로의 새 checkout에서
bootstrap, UI 빌드·타입 검사와 SDK·source bootstrap 검사 9개가 통과했습니다.
UI 검사 119개와 새 checkout의 ZIP 패키징 검사 128개가 통과했습니다.
기존 SDK와 새 SDK의 JS·타입 선언 산출물은 바이트 단위로 동일했습니다.

SQL Server 패치의 SDK 참조 3곳을 저장소 내부 경로로 바꾸고,
고정 base의 새 checkout에서 패치 적용과 최종 Git tree를 검증했습니다.
기존 SQL Server checkout은 UI 의존성 경로 두 파일과 bootstrap 메타데이터만 수정했습니다.
다른 staged 항목 43개는 보존했으며 새 SDK 경로로 frozen install이 통과했습니다.
기존 `tabularis-host` 폴더 제거 후 내부 SDK 검증과 설치된 앱의 드라이버 목록 조회가 통과했습니다.

이 검사는 DB 접속이나 쿼리를 실행하지 않았습니다.
이번 SDK 경로 전환에서 전체 수정 호스트 빌드, 다른 OS 실행과 레지스트리 수용 검증은 수행하지 않았습니다.
# 개인 경로 제거와 공식 가이드 확인 — 2026-10-07

[공식 Plugin Guide](https://github.com/TabularisDB/tabularis/blob/main/plugins/PLUGIN_GUIDE.md)와
[Plugin System 문서](https://tabularis.dev/wiki/plugins)의 UI 확장 계약을 다시 확인했습니다.
상대 자산 경로, IIFE 전역 이름·externals와 직접 Tauri 접근 금지를 대조했습니다.
공개 검증 문서에서 개인 실행 기록의 절대 경로를 제거했습니다.
패치의 임시 디렉터리는 운영체제 API로 만들고 PostgreSQL 통합 fixture는 PATH의 `psql`을 사용합니다.

고정 원본에 패치를 새로 적용해 SHA256과 최종 Git tree를 검증했습니다.
수정된 Node fixture 두 개의 문법과 Rust fixture 세 개의 구문을 확인했습니다.
bootstrap 검사 4개가 통과했습니다. 이번 경로 조정에서 전체 호스트 빌드와 DB 통합 검사는 실행하지 않았습니다.
SQL Server 작업 체크아웃의 staged 변경 45개는 Git 백업 복원을 검증한 뒤 작업 폴더를 제거했습니다.
로컬의 Tabularis 관련 작업 저장소는 Azure와 Spatial 두 개이며, 설치 연결 14개의 드라이버 참조를 확인했습니다.
이번 검사는 DB 접속이나 쿼리를 실행하지 않았고 레지스트리 수용 여부를 통과로 표시하지 않습니다.
