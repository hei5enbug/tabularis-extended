# Cosmos 연결과 문서 작업

이 안내는 저장된 연결에서 인증하고 쿼리와 문서 편집을 수행하는 절차를 설명합니다.

## 공식 0.26.0 기본 조회

연결 유형은 `cosmos-nosql`을 선택합니다. 추가 필드에 Cosmos NoSQL HTTPS 계정 주소,
기본 데이터베이스와 기본 컨테이너를 지정하세요. `extra`에는 공개 연결 설정만 저장합니다.
계정 키는 연결의 비밀번호 필드에 입력합니다. 키를 플러그인 설정, endpoint, 명령 인자에 넣지 마세요.

Azure CLI를 선택하면 이 컴퓨터에서 `az login`을 먼저 실행하고 의도한 테넌트 ID를 입력합니다.
드라이버가 해당 테넌트의 Cosmos 토큰을 메모리로 받아 사용합니다.
Azure CLI가 관리하는 로그인 캐시를 플러그인이 복사하거나 Git에 저장하지 않습니다.
Cosmos 데이터 읽기 권한은 별도로 필요합니다. Windows에서는 공식 Azure CLI 설치를 사용합니다.
Azure CLI 인증을 선택한 경우에만 CLI가 필요하며, 플러그인 프로세스의 `PATH`에서 찾습니다.
설치 디렉터리를 추측하지 않습니다. GUI 실행 환경에도 공식 CLI의 실행 디렉터리가 포함돼야 합니다.

Cosmos 작업 공간이나 SQL 편집기에서 `SELECT * FROM c`처럼 Cosmos SQL을 실행합니다.
`c`는 연결에 지정한 기본 컨테이너의 별칭입니다. SQL을 SQL Server 문법으로 변환하지 않습니다.
기존 테이블 뷰가 만드는 `SELECT * FROM "container"`는 Cosmos SQL이 아니므로 작업 공간을 사용하세요.
다른 컨테이너는 연결의 기본 컨테이너를 변경한 뒤 다시 연결합니다.

조회는 기본 100개이며 host가 지정한 limit과 기존 행·바이트·RU 상한을 적용합니다.
다음 페이지가 남으면 `truncated: true`로 표시합니다. 지원하지 않는 페이지 버튼은 활성화하지 않습니다.
화면에는 최대 20개 문서와 64 KiB의 샘플만 표시합니다.
연결을 바꾸거나 창을 닫은 뒤 도착한 결과는 화면에서 버립니다.
호스트의 timeout `cancel` 알림은 실행 중인 해당 요청에 전달합니다.

이 경로는 읽기 전용입니다. 문서 CRUD, continuation 재개, 앱 내 Entra 로그인과 서비스 주체 인증은
공통 서비스를 제공하는 수정 호스트에서 사용합니다. 실제 Azure DB 접근 검증은 별도입니다.

## 인증하기

공개 Azure Cosmos DB for NoSQL HTTPS endpoint를 사용합니다.
다른 API, 임의 proxy, TLS 검증을 끄는 설정은 지원하지 않습니다.
연결의 `auth_mode`는 아래 셋 중 하나입니다.

| 모드 | 준비와 전달 |
| --- | --- |
| `account_key` | 계정 키를 인증 화면의 보호된 입력으로 등록 |
| `entra_user` | tenant·client ID를 저장하고 인증 시작 후 사용자 인증 진행 |
| `entra_service_principal` | tenant·client ID를 저장하고 보호된 입력으로 client secret 등록 |

비밀이나 access token을 `extra`, 설정 JSON, URL, command 인자, 환경 변수에 저장하지 마세요.
인증 보관은 이번 세션 또는 기기 보안 저장소를 선택합니다.
보안 저장소를 사용할 수 없으면 세션 보관을 명시적으로 선택해야 합니다.
사용자 인증이 필요하면 GUI가 표시하는 인증 페이지와 코드를 사용합니다.
DB scope는 host가 `https://cosmos.azure.com/.default`로 고정합니다.

무인 호출의 보호된 credential 채널에는 host가 관리하는 credential 등록 경로를 사용합니다.
MCP JSON에 비밀을 넣어 `credential.import`를 호출하는 방식은 허용하지 않습니다.
Azure CLI의 실제 토큰 획득과 최소 읽기 쿼리는 확인했습니다. 문서 쓰기와 전체 갱신 검증은 미완료입니다.

## Azure CLI 로그인 사용하기

Microsoft Entra 사용자 인증에서 로그인 소스를 `Azure CLI (az login)`로 선택하세요.
개발 컴퓨터에서 먼저 `az login --tenant TENANT_ID`를 실행합니다.
연결 설정에는 `auth_mode: "entra_user"`, `auth_source: "azure_cli"`와 테넌트 ID가 들어갑니다.
클라이언트 ID는 공개 Azure CLI ID `04b07795-8ddb-461a-bbee-02f9e1bf7b46`로 고정됩니다.
공통 서비스 호스트에서는 호스트가 CLI를 실행하고 transient 토큰을 드라이버에 전달합니다.
공식 호스트의 기본 조회 경로에서는 드라이버가 CLI를 직접 실행해 토큰을 메모리로 받습니다.
두 경로 모두 공식 Cosmos SDK를 사용하며 CLI의 원본 오류와 토큰을 출력하지 않습니다.

Cosmos 데이터 접근 역할이 있어야 문서를 조회할 수 있습니다.
리소스 관리 권한과 데이터 접근 권한은 별개입니다. 앱 로그아웃은 CLI 세션을 종료하지 않습니다.
전체 동작과 보안 경계는 [공통 인증 안내](https://github.com/hei5enbug/tabularis-spatial/blob/main/docs/usage.md#azure-cli-로그인-사용하기)를 참고하세요.

## 작업 공간 열기

연결 폼의 Cosmos 추가 필드에서 endpoint와 인증 방식을 저장하세요.
Cosmos 작업 공간은 플러그인 설정의 actions 또는 Cosmos 연결·결과 화면의 actions에서 열 수 있습니다.
항상 저장된 연결을 명시적으로 선택합니다.
그다음 데이터베이스·컨테이너와 파티션 키 정보를 확인합니다.

임의 쿼리는 원문 SQL과 이름 있는 JSON parameter를 전달하며 값을 문자열로 강제 변환하지 않습니다.
기본 결과는 `json_values`여서 scalar, array, null과 projection을 그대로 유지합니다.
문서 모드는 후보를 point read하여 같은 ETag와 전체 JSON인지 확인한 경우에만 편집 가능한 문서로 표시합니다.

## 안전하게 문서 편집하기

기본은 읽기 전용입니다.
쓰기를 허용한 연결에서도 host의 명시적 승인 없이 쿼리 쓰기를 수행하지 않습니다.
드라이버의 직접 v1 경로도 제공된 읽기 전용 context에서 쓰기를 거부합니다.
실제 DB 권한까지 제한하려면 별도의 읽기 전용 주체를 관리자가 준비해야 합니다.

문서 식별자는 `id`와 메타데이터 순서대로 구성한 전체 typed 파티션 키입니다.
문자열·숫자·boolean·null·없는 속성은 서로 다릅니다.
계층형 키는 모든 구성 요소가 필요합니다.
기존 문서의 ID·파티션 키와 시스템 속성을 편집해서는 안 됩니다.
JavaScript 안전 정수 범위를 벗어난 정수 편집도 거부합니다.

새 문서는 명시적 ID로 생성하며 자동 ID와 upsert를 사용하지 않습니다.
replace·delete는 최신 ETag의 If-Match가 필요합니다.
`ETAG_CONFLICT`이면 최신 문서를 조회한 뒤 사용자가 다시 결정하세요.

쓰기 요청의 timeout·연결 손실·취소는 이미 적용됐는지 알 수 없을 수 있습니다.
`OUTCOME_UNKNOWN`이면 자동 재전송하지 말고 최신 문서 상태를 별도로 확인하세요.
취소 ACK나 로그아웃 ACK만으로 DB rollback이나 Azure token 폐기를 주장하지 않습니다.

## continuation과 비용

공개 continuation은 host가 관리하는 opaque token입니다.
SDK token과 남은 문서는 private cursor state이며 UI나 보고서에 노출하지 않습니다.
쿼리·parameter·연결·인증·컨테이너가 달라진 상태는 재개할 수 없습니다.
커서는 15분 TTL과 소유권 검증을 적용하며 로그아웃·무효화 뒤에는 재사용하지 않습니다.

SDK가 token을 제공하지 않으면 완료된 materialized 결과만 수용합니다.
10,000개·32 MiB 저장 상한과 더 작은 실제 RPC frame 상한에 걸리면 불완전 결과를 성공으로 바꾸지 않습니다.
기본 RU budget은 100이며 모든 fetch와 문서 검증 point read를 포함합니다.
관찰하지 못한 RU는 `null`로 보존하고 0이라고 표시하지 않습니다.
큰 공개 page와 private tail 때문에 실제 전송 상한이 저장 상한보다 먼저 적용될 수 있습니다.

## 실제 Azure 수동 검사

기존 CLI 로그인으로 수행한 최소 읽기 검사는 [검증 기록](verification.md#기존-cli-로그인의-읽기-전용-후속-검증)에 있습니다.
아래 harness는 읽기 전용 검사 명령이 아닙니다. 전용 기존 DB·컨테이너에 fixture 문서를 쓰므로
동의 flag 두 개가 필요합니다. 읽기 전용으로 한정한 이번 검증에서는 실행하지 않았습니다.
컨테이너 이름은 `tabularis_test_`로 시작하며 기본·계층형 컨테이너가 서로 달라야 합니다.
DB·컨테이너 생성·삭제, RU·index 정책 변경은 수행하지 않습니다.

```sh
pnpm test:live \
  --config /absolute/private/config.json \
  --credential-fd 3 --readonly-credential-fd 4 \
  --allow-live --allow-fixture-writes \
  --report /absolute/private/report.json
```

비밀이 없는 config 예시는 다음과 같습니다. 실제로 준비한 전용 리소스의 이름으로 바꿔주세요.
Entra 모드에는 `tenant_id`와 `client_id` UUID도 필요합니다.

```json
{
  "version": 1,
  "dedicated_test_resource": true,
  "endpoint": "https://YOUR-ACCOUNT.documents.azure.com",
  "database": "tabularis_validation",
  "container": "tabularis_test_basic",
  "hierarchical_container": "tabularis_test_hierarchical",
  "auth_mode": "account_key"
}
```

호출자는 서로 다른 FD 3..255를 보호된 방식으로 열어 각각의 transient credential JSON을 제공합니다.
FD 3에는 fixture 쓰기가 가능한 인증을, FD 4에는 실제 읽기 전용 인증을 제공합니다.
credential 값은 argv·stdin·환경 변수로 전달하지 않습니다.
설정에는 version·전용 리소스 확인·endpoint·database·두 container·auth mode와 필요한 tenant/client ID만 둡니다.
report 부모는 기존의 private 소유 디렉터리여야 하며 기존 report를 덮어쓰지 않습니다.
Windows에서는 기본 PowerShell로 설정 파일과 report 디렉터리의 ACL을 credential 읽기 전에 검사합니다.
소유자는 현재 사용자여야 하며, 접근을 허용하는 항목은 현재 사용자·SYSTEM·Administrators만 허용합니다.
상속된 접근 권한도 검사합니다. 권한을 확인하지 못하면 실행을 거부합니다.

harness는 자신의 run UUID로 표시한 문서만 확인하고 정리합니다.
native cross-process resume을 관찰하지 못하거나 cleanup 결과가 불명확하면 exit 4로 끝납니다.
pass인 경우에도 index 정책의 관리자 준비, 사용자 로그인·refresh, GUI·host parity는 별도 미관찰 항목입니다.
`integration_complete`는 false로 남으며 실제 전체 Azure 통합 완료를 뜻하지 않습니다.
