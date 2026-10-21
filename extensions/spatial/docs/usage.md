# GUI·CLI·MCP 사용법

## 공식 0.26.0 기본 지도

공식 앱에서는 PostgreSQL 테이블 툴바의 `Map` 버튼으로 지도를 엽니다.
툴바가 테이블 정보를 전달하지 않으면 스키마와 테이블 이름을 직접 입력합니다.
테이블 정보가 있는 호스트에서는 해당 테이블을 바로 엽니다. 모달을 연 연결이 바뀌면 조회를 중단합니다.
기존 PostgreSQL 드라이버가 생성된 읽기 전용 SQL을 실행하며 원본 도형은 수정하지 않습니다.
공간 컬럼은 PostGIS의 `public.geometry_columns`와 `public.geography_columns`에서 찾습니다.
다른 스키마에 PostGIS 확장이 설치돼 있으면 이 기본 경로는 지원하지 않는다는 DB 오류를 반환합니다.

기본 지도는 테이블을 새로 조회합니다. SQL 편집기 결과 스냅샷이나 기존 SQL을 변형해 표시하지 않습니다.
원본 SRID가 0인 행에는 사용자가 지정한 SRID를 표시 계산에만 적용하고 `ST_Transform`으로 변환합니다.
원본 geometry·geography와 좌표는 유지합니다.

표시 상한은 행 1,000개, 좌표 100,000개, GeoJSON 8 MiB입니다.
호스트의 더 작은 조회 상한도 적용됩니다. 지도에는 조회한 표본만 나타날 수 있습니다.
도형 속성을 확인하고 표시용 GeoJSON을 내려받을 수 있습니다.
기본 배경은 외부 지도 요청이 없는 빈 배경입니다.
연결이 달라진 뒤 도착한 이전 결과는 표시하지 않습니다.
0.26.0의 공개 UI query hook은 진행 중인 DB 요청을 취소하는 API를 제공하지 않습니다.

지도 상태 저장, 다중 레이어 관리, 실행 결과 스냅샷, MCP·CLI 지도 조작은 아래의 공통 서비스 경로를 사용합니다.
호스트 버전 문자열 외에 실제 서비스·asset API 제공 여부를 확인합니다.

## 지도

수정 호스트에서 패치된 PostgreSQL 드라이버와 Spatial ZIP을 활성화합니다.
기존 PostgreSQL 연결을 선택하고 PostGIS 공간 컬럼이 있는 쿼리를 실행합니다.
결과 도구 모음의 `Map`에서 공간 컬럼을 선택합니다.
테이블 탐색에서는 테이블과 공간 컬럼을 지정해 현재 지도 범위로 조회합니다.

쿼리 결과 지도는 저장된 원본 snapshot을 사용합니다.
열 이름이 중복돼도 result set 번호와 열 번호로 구분합니다.
테이블 지도는 현재 데이터이므로 저장된 쿼리 결과와 시점이 다를 수 있습니다.
객체 상세는 원본 식별자를 유지하며, 행 편집 후 새 조회는 별도 결과를 만듭니다.
객체 선택 목록은 100개씩 표시합니다. 이전·다음 버튼이나 페이지 번호로 모든 객체에 접근할 수 있습니다.
`더 보기`는 DB의 다음 결과 페이지를 추가하며, 지도에는 누적된 전체 객체를 표시합니다.

SRID가 없는 데이터는 SRID 입력 전까지 변환하지 않습니다.
날짜변경선 처리는 `preserve` 또는 `shortest`를 명시적으로 선택합니다.
GeoJSON 내보내기는 지도 표시용 좌표를 저장하며 DB 원본을 변경하지 않습니다.
파일 저장 창을 취소하면 저장 성공으로 처리하지 않습니다.

저장된 지도나 CLI·MCP 요청에 외부 배경지도 URL이 있어도 처음에는 단색 배경으로 표시합니다.
Basemap에서 네트워크 사용과 제공자 라이선스 표시를 확인한 뒤 `Basemap 적용`을 눌러야 요청합니다.
동의는 현재 창의 해당 URL에만 적용합니다. 승인한 URL과 다른 URL이나 새 창에서는 재확인해야 합니다.
확인란을 해제하면 단색 배경으로 돌아갑니다.
동의 대기로 배경지도를 적용하지 못한 상태는 `gui_applied: false`, `rendered_version: null`로 응답합니다.

## CLI

수정 호스트 실행 파일에 `service` 하위 명령이 추가됩니다.
각 요청은 공통 JSON envelope를 사용합니다. 아래 내용을 `connections.json`으로 저장합니다.

```json
{
  "protocol_version": 1,
  "operation": "connection.list",
  "request_id": "list-connections-1",
  "connection_id": null,
  "session_id": null,
  "map_id": null,
  "expected_version": null,
  "deadline_ms": 30000,
  "input": {}
}
```

```sh
tabularis service --request connections.json
tabularis service --stdio
tabularis --mcp
```

`--stdio`는 한 줄에 요청 하나를 받고 한 줄에 JSON 응답 하나를 반환합니다.
결과 페이지, session, 인증 작업을 이어서 사용할 때는 이 프로세스를 유지합니다.
일회성 `--request` 프로세스가 끝나면 그 프로세스가 소유한 임시 결과와 session도 종료됩니다.
요청마다 고유 `request_id`와 명시적 `connection_id`를 사용합니다.
현재 GUI에서 선택한 연결에 의존하지 않습니다.

| 작업 | 사용 방법 |
|---|---|
| 쿼리 | `query.execute`, 정확한 SQL과 명시적 database·parameters |
| 저장 결과 페이지 | `result.get`, 원래 `result_id`·set 번호·offset |
| 결과 지도 | `spatial.query_result`, 결과·set·공간 컬럼 번호 |
| 테이블 지도 | `spatial.table_query`, 테이블·공간 컬럼·viewport |
| 내보내기 | `spatial.export`, 결과 또는 테이블 source |
| 작업 취소 | `job.cancel`, 응답에서 받은 작업 식별자 |
| 지도 변경 | `map.*`, `map_id`와 변경 전 `expected_version` |

전체 입력 계약은 `build-support/sdk/packages/service-contracts/schema/v1/request.json`에 있습니다.
대량 결과는 페이지·요약·파일 참조로 받습니다. 응답의 `limits.truncated`와 `limits.reasons`를 확인하세요.
GeoJSON artifact는 다음 명령으로 아직 존재하지 않는 절대 경로에 저장합니다.

```sh
tabularis service artifact-save artifact-UUID --output /absolute/new-file.geojson
```

`map.open`과 화면 반영을 기다리는 지도 변경은 `gui_instance_id`도 필요합니다.
실행 중인 GUI의 capabilities에서 ID를 받거나 `GUI_UNAVAILABLE` 오류의 `details.instances`를 확인합니다.
지도 상태 저장 성공과 화면 적용 성공은 구분합니다. `gui_applied: true`는 실제 GUI의 적용 응답을 받은 경우입니다.
GUI가 없어도 DB 조회와 GeoJSON 내보내기를 실행할 수 있습니다.

## MCP

MCP 클라이언트의 stdio 서버 명령을 수정 호스트 실행 파일, 인자를 `["--mcp"]`로 설정합니다.
`tools/list`는 공통 operation 도구와 기존 도구를 함께 반환합니다.
공통 도구의 이름은 `query.execute`, `spatial.export`, `map.open`처럼 operation 이름과 같습니다.
각 `arguments`에는 위와 같은 전체 envelope를 넣습니다.
기존 `run_query` 등도 유지하며, Cosmos 쿼리는 database와 container를 명시해야 합니다.

MCP 결과에는 요약·소량 sample·페이지 정보·artifact 참조가 포함됩니다.
파일은 `tabularis://artifact/ID`와 제한된 chunk resource로 읽습니다.
원본 공간 좌표 전체를 대화 응답으로 반환하지 않습니다.

## 쓰기와 인증

기본은 읽기 전용입니다. SQL 첫 단어로 쓰기 권한을 판단하지 않습니다.
쓰기 허용 연결에서 GUI 쓰기 옵션, CLI `--write`, MCP `query.execute_write`로 쿼리 쓰기 의도를 명시합니다.
승인 요청이 생기면 GUI에서 승인하거나 다음 명령을 사용합니다.

```sh
tabularis service approve APPROVAL_ID --decision approve
tabularis service approve APPROVAL_ID --decision deny
```

비밀번호·키·토큰은 명령 인자나 요청 JSON에 넣지 않습니다.
GUI의 보호 입력, CLI의 `--credential-stdin`, Unix의 `--credential-fd`로 전달합니다.
`--credential-stdin`은 요청 파일과 함께 쓰며 `--request -` 또는 `--stdio`와 함께 사용할 수 없습니다.
`--stdio --credential-fd`는 소비할 요청을 `--credential-request-id`로 고정합니다.
Windows에서는 파일 서술자 입력 대신 보호된 stdin을 사용합니다.

Entra 사용자 로그인은 로그인·MFA 과정을 거치고, 무인 자동화는 앱 자격 증명을 사용합니다.
Azure CLI의 실제 SQL·Cosmos 토큰 획득과 최소 읽기를 확인했습니다.
SQL 플러그인의 읽기 전용 권한 확인과 전체 인증·갱신·CRUD 검증은 미완료입니다.
취소 응답의 `outcome`이 불명확하면 쓰기를 자동 재실행하지 말고 대상 DB 상태를 확인합니다.


## Azure CLI 로그인 사용하기

개발 컴퓨터에서 Azure CLI 2.54 이상을 설치하고 필요한 테넌트로 로그인합니다.
포털의 로그인과 CLI의 로그인은 별도 세션입니다. 테넌트 ID는 실제 값으로 바꾸세요.

```sh
az login --tenant TENANT_ID
```

SQL Server와 Cosmos 연결의 Microsoft Entra 사용자 인증에서 로그인 소스를
`Azure CLI (az login)`로 선택합니다. 클라이언트 ID는 공식 Azure CLI의 공개 ID로 고정됩니다.
GUI에서 저장한 연결은 MCP와 CLI에서도 같은 공통 인증 서비스를 사용합니다.
프로그램으로 연결을 등록한다면 공개 설정의 `extra`에 아래 필드를 넣습니다.

```json
{
  "auth_mode": "entra_user",
  "auth_source": "azure_cli",
  "tenant_id": "TENANT_ID",
  "client_id": "04b07795-8ddb-461a-bbee-02f9e1bf7b46"
}
```

앱은 `az account get-access-token`을 셸 없이 실행해 토큰을 메모리에서 기존 드라이버로 전달합니다.
CLI의 오류 출력과 토큰은 공통 서비스 응답이나 앱 로그에 넣지 않습니다.
SQL과 Cosmos의 토큰 대상은 각각 고정하며 테넌트·클라이언트·주체·만료 정보를 확인합니다.
토큰의 실제 서명과 데이터 접근 권한은 대상 Azure 서비스가 검증합니다.
현재 어댑터는 해당 메타데이터를 확인할 수 있는 JWT 토큰을 지원합니다.
CLI가 불투명한 토큰을 반환하면 `AUTH_REQUIRED`를 반환합니다.

앱은 CLI의 access token이나 refresh token을 별도 파일이나 키체인에 저장하지 않습니다.
Azure CLI는 자신의 로그인 캐시를 관리합니다. `.azure/`, `azure-local/`, `.env`와 인증 파일은 Git에서 제외합니다.
앱의 로그아웃은 해당 연결의 인증 상태를 정리하며 CLI의 로그인 세션은 종료하지 않습니다.
CLI 계정을 바꾸면 다음 호출에서 주체 변경을 감지해 이전 연결의 인증을 무효화합니다.

SQL에는 Entra 데이터베이스 사용자가 필요하며 Cosmos에는 데이터 접근 역할이 필요합니다.
포털의 리소스 관리 권한만으로 문서 조회가 허용되지는 않습니다.
CLI 로그인은 개발과 수동 검증에 사용하고 무인 작업은 `entra_service_principal`을 사용합니다.
