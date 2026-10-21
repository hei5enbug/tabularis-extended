# 검증 현황과 재현

이 문서는 monorepo의 `extensions/spatial`에서 직접 실행한 검사와 실행하지 않은 검사를 구분합니다.
`native`는 해당 OS에서 실제 실행했다는 뜻이고, `synthetic`은 대체 서버·드라이버 또는 플랫폼 입력을 사용했다는 뜻입니다.

## monorepo 전환에서 확인한 것

2026-10-21 macOS ARM64에서 Node `24.21.0`, pnpm `10.30.3`, Rust `1.96.0`으로 실행했습니다.
처음 다섯 명령은 `extensions/spatial`에서, 나머지는 저장소 루트에서 실행했습니다.

| 명령 | 결과 |
|---|---|
| `pnpm test:sdk` | 5개 통과 |
| `pnpm test:ui` | 12개 파일, 124개 통과 |
| `pnpm typecheck:ui` | 오류 없음 |
| `cargo test --locked --workspace` | 68개 통과 (테스트 묶음 4개: 9, 20, 22, 17개) |
| `node --test tests/install/*.test.mjs` | 128개 통과 |
| `cargo build --locked --release --manifest-path extensions/postgresql/Cargo.toml --bin postgresql-plugin` | 빌드 성공 (저장소 루트에서 실행, 출력 디렉터리는 임시 위치) |
| `cargo build --locked --release --manifest-path extensions/sqlserver-plus/Cargo.toml --bin sqlserver-plugin` | 빌드 성공 (같은 조건) |
| 두 워크플로 YAML 파싱 | `ext-spatial.yml`, `ext-azure.yml` 모두 파싱 성공 |

UI 검사 124개 중 5개는 `ui/tests/routing.test.tsx`입니다.
서비스 API가 있는 호스트에서 `postgresql-plus`는 서비스 도구 모음으로, `postgres`·`postgresql`은 기본 도구 모음으로 가고
다른 드라이버에는 아무것도 표시하지 않는 분기를 확인합니다. 이 검사는 두 도구 모음을 대체 컴포넌트로 바꿔서 실행합니다.

이번에 실행하지 않은 검사는 다음과 같습니다.

- 저장소 루트 호스트의 빌드와 Rust 테스트, 실제 설치기 검사. 다른 작업이 호스트를 소유합니다.
- `.github/workflows/ext-spatial.yml`의 GitHub Actions 실행. YAML만 파싱했습니다.
- 실제 PostGIS 검사, WKWebView 검사, 실제 앱 GUI·CLI·MCP 검사.
- `extensions/postgresql`, `extensions/sqlserver-plus`의 테스트와 clippy.

## 재실행

일반 검사는 Azure 자격 증명을 요구하지 않습니다. `extensions/spatial`에서 실행합니다.

```sh
pnpm test:sdk
cargo test --locked --workspace
pnpm test:ui
node --test tests/install/*.test.mjs
```

호스트의 공통 서비스 검사는 저장소 루트에서 실행합니다. 이 명령은 이번에 실행하지 않았습니다.

```sh
cargo test --locked --manifest-path src-tauri/Cargo.toml --lib services::
```

호스트의 실제 PostGIS 검사는 `TABULARIS_TEST_POSTGIS_PLUGIN`에 `extensions/postgresql`에서 빌드한
`postgresql-plugin`의 절대 경로를, `TABULARIS_TEST_POSTGIS17_PORT`와 `TABULARIS_TEST_POSTGIS18_PORT`에
전용 로컬 fixture 포트를 지정합니다. DB는 `spatial_fixture`, 사용자는 `postgres`, 접속 주소는 `127.0.0.1`입니다.
두 fixture 모두 PostGIS 확장이 필요합니다. 실제 검사는 임시 스키마를 생성하므로 전용 테스트 DB에서 실행합니다.

```sh
cargo test --locked --manifest-path src-tauri/Cargo.toml \
  --lib service_spatial::tests::live -- --ignored --test-threads=1
cargo test --locked --manifest-path src-tauri/Cargo.toml \
  --lib service_spatial::tests::export_live -- --ignored --test-threads=1
```

명시 실행에서 입력이 없으면 실패합니다. 일반 unit 실행의 `ignored`는 실제 DB 검증 통과를 뜻하지 않습니다.
실제 앱 검사는 `TABULARIS_TEST_HOST_BINARY`, `TABULARIS_TEST_POSTGIS_PLUGIN`, `TABULARIS_TEST_POSTGIS_MANIFEST`,
`TABULARIS_TEST_POSTGIS17_PORT`, `TABULARIS_TEST_SPATIAL_ZIP`을 전용 로컬 fixture의 절대 경로·포트로 지정한 뒤
저장소 루트에서 실행합니다. ZIP 옆의 `.sha256` 파일도 필요합니다.

```sh
pnpm test:service:parity
pnpm test:spatial:webview
```

호스트는 격리한 식별자로 빌드해야 하며 기존 사용자 앱을 검사용으로 종료하거나 조작하지 않습니다.
WKWebView 성능 검사는 `extensions/spatial`에서 `node tests/integration/wkwebview/run.mjs --performance`로 실행합니다.
이 검사는 실제 MapLibre·WebGL을 사용하지만 서비스는 mock이므로 실제 앱 검사와 구분합니다.

## 아직 구분해야 하는 완료 조건

- 최소 읽기 확인과 전체 수용 검사를 구분합니다. SQL의 읽기 전용 주체 확인, MFA/앱 인증·갱신·CRUD와
  Cosmos 교차 파티션 정렬·집계·페이지 재개는 미완료입니다.
- GitHub Actions는 수동 실행하는 선택 사항입니다. 실제 OS에서 직접 실행한 검증도 환경과 결과를 기록해 근거로 씁니다.
  workflow 작성이나 synthetic 검사를 native 실행으로 간주하지 않습니다.
- 호스트 전체 strict clippy는 이전 호스트의 기존 코드 경고 때문에 실패했습니다. 전체 clippy 통과라고 주장하지 않습니다.
- 라이브 레지스트리 스키마 조회는 HTTP 403으로 실패했으므로 UI 전용 매니페스트의 레지스트리 수용 여부는 미확인입니다.

## 변경 이력

아래 기록은 패치 방식의 수정 호스트와 별도 checkout으로 빌드하던 시기의 결과입니다.
저장소 루트의 `0.27.0+extended.1` 호스트와 `extensions/` 아래 드라이버에서는 다시 실행하지 않았습니다.
그 시기의 패치 적용·Git tree 검증과 SDK 경로 전환 기록은 해당 절차와 함께 폐기해 옮기지 않았습니다.

### 이전 호스트에서 확인한 동작

| 영역 | 실행 환경과 결과 | 증거의 범위 |
|---|---|---|
| 공간 모델 | Spatial Rust core 68개 통과 | 원본 식별·좌표/행/바이트 제한·지도 버전 |
| PostgreSQL 드라이버 | unit 402개, 실제 PostGIS 74개 통과 | 원본 EWKB·geometry/geography·SRID·Z/M·NULL/EMPTY·읽기 전용·취소 |
| 공통 서비스 공간 조회 | 실제 PG17/18 조회 4개, PG18 대용량 export 1개 통과 | 불변 쿼리 결과·원 SQL 재실행 0회·변환·페이지·연결별 취소·32 MiB 초과 명시 export |
| 지도 UI | React 검사 97개, typecheck/build 통과 | 레이어·속성·선택·상태·오래된 응답 차단·artifact 저장 호출 |
| WebView 렌더링 | macOS WKWebView 22개·열기/닫기 두 번 통과 | 실제 WebGL·로컬 CSS/worker·모달 열기/닫기. 서비스는 mock |
| 대용량 지도 | 실제 WKWebView에서 10,000 feature·250,000 coordinates 통과 | 최대 rAF 간격 50 ms, 100 ms 초과 0회 |
| 실제 앱 통합 | 실제 CLI/MCP parity 1개와 Tauri GUI 통합 1개 통과 | 원본 결과·페이지·export 비교, 별도 프로세스 지도 적용 ACK·선택·버전 충돌 |
| 공통 transport | 전체 서비스 검사 204개 통과, 기존 자식 프로세스 검사 1개 별도 실행 | operation 스키마·정책·결과·오류·CLI/MCP registry·실제 로컬 IPC |
| SQL Server 드라이버 | unit 244개·conformance 3개·synthetic Azure 33개·clippy 통과 | Entra 사용자/앱 인증 경로·토큰 갱신·TLS 검증·원본 SQL·CRUD |
| Cosmos | unit 625개·protocol 14개·synthetic live harness 74개 통과 | 공식 SDK의 문서/페이지/RU/429/ETag 계약 |
| 실제 설치 | macOS ARM64와 Rosetta x64 Cosmos ZIP, Spatial ZIP을 호스트 설치기로 검사 | ZIP 모든 파일의 SHA·권한·asset MIME, 동봉 Node 실행 |
| OS 공통 설치 | macOS ARM·Intel와 Linux x64 실제 호스트 설치 통과 | Windows는 검사 실행 파일 manifest 보정 후 통과 |

각 행은 해당 변경 시점의 검사이며 서로 다른 실행의 통과 수를 합산하지 않습니다.
Windows 2022의 검사 실행 파일은 처음에 `0xc0000139 (STATUS_ENTRYPOINT_NOT_FOUND)`로 종료됐습니다.
`rfd`가 Common Controls v6를 요구하지만 Tauri 기본 리소스가 검사 실행 파일에는 연결되지 않았기 때문입니다.
CI가 같은 manifest를 넣도록 보정했고 Windows 단독 실행에서 설치 검사가 통과했습니다.

### 실제 fixture

| 환경 | 고정 버전 | 이미지 digest |
|---|---|---|
| PG17 | PostgreSQL 17.7 / PostGIS 3.5.2, ARM64 | `sha256:bac8128cd62a35471a1e97966861aa2ac88c8d2f408767a425ae70695a607c99` |
| PG18 | PostgreSQL 18.6 / PostGIS 3.6.4, amd64 emulation | `sha256:7e00e8c3539fdd43f513b98806c8204714dcd09dea683c259e333d7690317119` |
| 로컬 SQL Server | SQL Server 2022 Developer 16.0.4245.2, amd64 emulation | `sha256:49b45a911dc535e9345fbfd7101a1bd8a1e190a5f29b877ef75387a061e5fcf0` |
| 실제 WebView | macOS 26.5 (25F71), WebKit 21624.2.5.11.4 | React 19.2.4, MapLibre 6.11.2 |

emulation 결과로 native 성능을 추정하지 않습니다.
로컬 fixture는 검사용 임시 스키마만 생성·정리했고 기존 사용자 DB나 Azure 자원을 사용하지 않았습니다.

### 2026-10-04 최종 통합 실행

macOS ARM64에서 사용자 프로필과 분리한 실제 실행 파일을 사용했고 실제 Azure에는 접속하지 않았습니다.

| 검사 | 관찰 결과 |
|---|---|
| CLI/MCP parity | 실제 PostGIS 쿼리·페이지·공간 변환·artifact 파일 내용 동일, 쓰기 거부 |
| 실제 GUI·CLI·MCP | 시작 직후 첫 지도 요청과 이후 조작 모두 적용 완료 ACK, 충돌 시 기존 상태 유지 |
| 대용량 WKWebView | 전체 1만 객체·25만 좌표, 목록 100개/페이지, 마지막 객체 접근, 100 ms 초과 0회 |
| 관련 회귀 | UI 47개, 데이터 제한 11개, 호스트 서비스 훅 17개 통과 |

전체 결과의 반복 전송을 새 페이지의 증분 갱신으로 바꿨습니다. 성능 fixture의 크기와 100 ms 기준은 유지했습니다.
최초 성능 실패와 시작 직후 GUI 요청 유실도 원본 로그에 보존했습니다.

### 2026-10-04 Azure CLI 어댑터

로그인된 포털과 같은 사용자로 Azure CLI 로그인과 리소스 설정 조회를 확인했습니다.
앱의 Rust 어댑터로 SQL·Cosmos 토큰 획득을 실제로 실행했고 두 대상 모두 통과했습니다.
Cosmos 토큰의 대상 값은 공개 리소스 ID로 반환되어 Cosmos 연결에만 허용했고 SQL에는 허용하지 않습니다.
실제 SQL 테스트 DB에 대한 읽기 전용 연결은 `AUTH_REQUIRED`로 실패했습니다.
실제 Cosmos 개발 계정의 메타데이터 조회는 HTTP 403이었고 IP 방화벽에 따른 `network_access` 거부를 확인했습니다.
그룹을 통한 데이터 역할 적용 여부는 확인하지 못했으므로 읽기 권한이 없다고 단정하지 않습니다.
역할이나 방화벽은 변경하지 않았고 DB나 문서의 생성·수정·삭제는 실행하지 않았습니다.

### 2026-10-06 공식 0.26.0 기본 경로

공식 macOS ARM64 Tabularis 0.26.0 프로필에 UI 전용 패키지를 설치하고 Plugin Center에서 활성화를 확인했습니다.
로컬 PostgreSQL 17.7·PostGIS 3.5.2의 작업 전용 테이블을 공식 앱에서 조회했습니다.
SRID 4326의 점·선·면이 실제 WebGL 지도에 3 features·8 coordinates로 표시됐고
점 선택 시 `id: 1`과 한글 속성 `label: "서울 점"`도 확인했습니다. 검사 후 작업 전용 연결과 테이블을 제거했습니다.
공식 Plugin Guide와 Building Plugins 문서를 대조해 IIFE 전역 `__tabularis_plugin__`, 상대 자산 경로,
Tauri 직접 호출 금지와 `data-grid.toolbar.actions` 슬롯 계약을 확인했습니다.
`app.map.renderer`와 `service_protocol`은 서비스 API가 있을 때만 쓰는 확장입니다.

### 2026-10-06 Azure SQL의 읽기 전용 후속 검증

기존 CLI 로그인으로 SQL 테스트 DB 두 곳에 pyodbc 5.3.0·Microsoft ODBC Driver 18로 접속했습니다.
`Encrypt=yes`, `TrustServerCertificate=no`, `ApplicationIntent=ReadOnly`에서 `SELECT 1`이 1을 반환했습니다.
ODBC 성공을 SQL 플러그인의 성공으로 대신하지 않습니다.

| 경로 | 관찰 결과 | 확인한 원인과 한계 |
|---|---|---|
| ODBC의 테스트 DB 두 곳 | `SELECT 1` 성공 | 로그인·네트워크·TLS 확인. DB 주체가 읽기 전용이라는 증거는 아님 |
| SQL 플러그인의 첫 테스트 DB | `CAPABILITY_UNAVAILABLE` | 권한 조사 결과가 `readonly::probe`의 10,000행 상한을 초과 |
| SQL 플러그인의 다른 테스트 DB | `WRITE_NOT_ALLOWED` | DB 주체를 읽기 전용으로 입증할 수 없음 |
| 동일 권한 probe의 ODBC 조회 | `INSERT`·`UPDATE`·`DELETE`와 DDL 권한 확인 | 두 DB 모두 현재 주체가 읽기 전용 요구를 충족하지 않음 |

권한·역할·방화벽·리소스를 변경하지 않았고 검사 상한과 읽기 전용 보호도 완화하지 않았습니다.
서비스 주체 인증·앱 내 사용자 인증·갱신·CRUD·전체 GUI/CLI/MCP 비교는 미완료입니다.

### 2026-10-06 외부 배경지도 동의와 보안 검사

저장된 지도와 CLI/MCP 지도 상태의 URL은 이전 렌더러에서 UI 확인 없이 로드될 수 있었습니다.
`MapEngine.apply`는 현재 모달에서 승인한 URL만 MapLibre에 전달하도록 보완했습니다.
동의 전에도 공간 레이어는 단색 배경으로 표시하고, 승인한 URL과 다른 URL·새 창에서는 다시 확인합니다.
회귀 검사는 동의 전 차단·해당 URL 적용·다른 URL 차단·동의 철회를 확인합니다.
Vite 7.3.6·Vitest 4.1.11로 갱신하고 Vite의 esbuild를 0.28.1로 고정했으며 `pnpm audit`은 모든 심각도에서 0건이었습니다.
Gitleaks 8.30.1의 탐지는 합성 fixture뿐이었고 실제 비밀 값은 발견하지 못했습니다.
