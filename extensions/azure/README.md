# Tabularis Azure

Tabularis의 Azure 연동을 위한 저장소입니다.
현재 Cosmos DB for NoSQL 드라이버, 문서 작업 공간과 Entra·Azure CLI 인증 UI를 제공합니다.
Azure SQL은 기존 SQL Server 플러그인을 재사용합니다. 공통 Rust 인증 서비스와 SQL Server 연동 패치는
[tabularis-spatial](https://github.com/hei5enbug/tabularis-spatial)의 `integration/`에서 제공합니다.
저장된 연결과 설치 호환성을 위해 Cosmos 드라이버의 식별자는 `cosmos-nosql`로 유지합니다.
MongoDB API와 SQL 트랜잭션 세션은 지원하지 않습니다.
플러그인 버전은 `0.1.0`이며 Tabularis `0.26.0` 이상에서 기본 조회를 지원합니다.
문서 수정과 페이지 재개는 공통 서비스 protocol `1`을 제공하는 수정 호스트 `0.26.1-spatial.1`이 필요합니다.
공개 플러그인 SDK API 버전은 `0.2.0`입니다.

## 빌드와 설치

이 저장소만 checkout하면 됩니다. 다른 private 저장소나 PAT는 필요하지 않습니다.
공개 npm 의존성을 받으려면 네트워크 또는 채워진 패키지 cache가 필요합니다.
Node `24.21.0`, pnpm `10.30.3`, Rust `1.96.0`을 준비한 뒤 실행하세요.

```sh
pnpm bootstrap
pnpm verify:sdk
pnpm typecheck
pnpm --dir ui typecheck
pnpm test:bootstrap
pnpm test:sdk
```

공개 SDK 입력은 `build-support/sdk`에 22개 파일로 고정되어 있습니다.
bootstrap은 이 원장을 확인한 뒤 root workspace만 frozen install하고 계약, SDK, 드라이버, UI를 빌드합니다.
다른 저장소를 만들거나 수정하지 않습니다.
고정 source와 생산 ZIP을 만드는 방법은 [빌드 안내](docs/building.md#고정-sdk-입력)를 참고하세요.

| 패키지 대상 | 현재 실행 근거 |
| --- | --- |
| macOS arm64 | 생산 ZIP의 실제 native 설치와 bundled Node 실행 확인 |
| macOS x64 | 실제 Rosetta 실행과 Intel native CI의 ZIP/runtime 실행 확인 |
| Linux arm64 | Docker ARM64에서 패키지 launcher와 동봉 Node 실행 확인 |
| Linux x64 | Ubuntu native CI의 ZIP/runtime 실행 확인 |
| Windows x64 | Windows 2022 native CI의 ZIP/runtime 실행 확인 |

## 호스트별 지원

| 기능 | 공식 0.26.0 이상 | 공통 서비스 호스트 |
| --- | --- | --- |
| 계정 키·Azure CLI 사용자 인증 | 연결 비밀번호 또는 Azure CLI 사용 | 보호된 공통 인증 서비스 사용 |
| DB·컨테이너 탐색과 Cosmos SQL | 기본 컨테이너 조회, 읽기 전용 | 명시한 컨테이너 조회 |
| continuation·문서 CRUD | 지원하지 않음 | 권한과 ETag를 확인해 지원 |
| 앱 내 Entra 사용자·서비스 주체 인증 | 지원하지 않음 | 지원 |

버전 문자열 외에 실제 UI API 제공 여부로 지원 경로를 선택합니다.
새 호스트 버전의 전체 호환성을 자동으로 보장하는 것은 아닙니다.
기본 조회 설정은 [사용 안내](docs/usage.md#공식-0260-기본-조회)에 있습니다.

## 연결과 작업 공간

연결 설정에는 공개 `https://<account>.documents.azure.com` endpoint와 데이터베이스를 입력합니다.
계정 키 또는 Entra 사용자·서비스 주체 인증을 선택할 수 있습니다.
비밀은 연결 설정 JSON에 넣지 않고 인증 화면의 보호된 전달 기능으로 등록합니다.

Cosmos 작업 공간에서 저장된 연결을 명시적으로 선택한 뒤 데이터베이스와 컨테이너를 확인하세요.
임의 쿼리 결과는 원래 JSON 값으로 표시합니다.
문서를 편집하려면 전체 파티션 키와 최신 ETag를 확인해야 합니다.
쓰기에는 연결의 쓰기 허용과 host의 명시적 승인이 필요합니다.

인증, 읽기 전용, continuation과 결과 불명 처리 방법은 [사용 안내](docs/usage.md#안전하게-문서-편집하기)에 있습니다.
기존 Azure CLI 로그인으로 실제 개발 계정의 DB·컨테이너·컬럼 탐색과 최소 읽기 쿼리를 확인했습니다.
문서 CRUD·페이지 재개·앱 내 로그인과 갱신을 포함한 전체 Azure 검증은 아직 완료하지 않았습니다.
레지스트리 스키마 조회는 HTTP 403으로 막혀 등록 수용 여부도 미확인입니다.
GitHub Actions는 필요할 때 수동으로 실행하는 선택 사항이며 필수 완료 조건이 아닙니다.
실제 OS에서 직접 수행한 검증도 실행 환경과 결과를 기록하면 근거로 사용합니다.
mock 검사, 실제 설치, OS별 CI를 나눈 기록은 [검증 기록](docs/verification.md#관찰한-검사)를 참고하세요.

## 라이선스

[Apache License 2.0](LICENSE)으로 배포합니다.
생산 ZIP에는 bundled Node와 의존성의 라이선스 및 출처 원장이 포함됩니다.
