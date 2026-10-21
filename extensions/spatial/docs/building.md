# 빌드와 설치

## 독립 플러그인 빌드

Spatial 플러그인은 저장소에 고정한 SDK snapshot을 사용합니다. 앱이나 드라이버 checkout은 필요하지 않습니다.
Node `24.21.0`과 pnpm `10.30.3`으로 다음 명령을 실행합니다.

```sh
pnpm bootstrap
```

이 명령은 SDK snapshot을 검증하고 frozen lockfile로 의존성을 설치한 뒤 서비스 계약, plugin API,
UI를 빌드합니다. SDK 파일을 검증하려면 `pnpm verify:sdk`, SDK 패키지만 다시 빌드하려면
`pnpm build:sdk`를 실행합니다.

## 공식 UI 확장 계약

[공식 플러그인 가이드](https://github.com/TabularisDB/tabularis/blob/main/plugins/PLUGIN_GUIDE.md)와
[플러그인 시스템 문서](https://tabularis.dev/wiki/plugins)의 UI 확장 계약을 따릅니다.
모듈·자산은 플러그인 폴더 기준 상대 경로이며, UI는 `__tabularis_plugin__` IIFE로 빌드합니다.
React·JSX runtime·plugin API는 호스트의 전역 구현을 사용하고 Tauri API를 직접 호출하지 않습니다.
개인 실행 경로·개인 도구 설치 위치는 빌드 입력이나 배포 패키지에 포함하지 않습니다.

Spatial은 DB 실행 파일이 없는 UI 전용 패키지입니다.
`kind: driver`와 UI 확장만 가진 패키지의 로딩은 공식 0.26.0 호스트에서 확인했습니다.
이는 가이드의 표준 DB 드라이버 실행 파일 모델과 별도이며, 레지스트리 수용 여부는 미확인입니다.

## 앱과 기존 드라이버 source 준비

수정된 앱이나 기존 드라이버 source가 필요한 통합 작업에만 실행합니다.
`node scripts/bootstrap.mjs`는 아래 세 디렉터리를 Spatial 저장소 옆에 생성합니다.

| 디렉터리 | upstream |
|---|---|
| `tabularis-app-source` | `TabularisDB/tabularis` |
| `tabularis-postgresql-plugin` | `TabularisDB/tabularis-postgresql-plugin` |
| `tabularis-sqlserver-plugin` | `TabularisDB/tabularis-sqlserver-plugin` |

정확한 base commit, source commit, 패치 SHA256과 최종 Git tree는
[`integration/upstreams.json`](../integration/upstreams.json)에 고정돼 있습니다.
`base_commit`은 공식 upstream의 기준 revision이고, `source_commit`은 원래 수정 소스의 revision입니다.
SQL Server의 `patch_adaptations`는 UI SDK 링크를
이 저장소의 고정 SDK로 바꾼 내용을 기록하며, 이 조정은 패치 SHA256과 최종 Git tree에 반영됩니다.
부모 경로에 기존 checkout이 있으면 새 부모 디렉터리를 사용하세요.
bootstrap은 원래 저장소에 commit하거나 upstream에 push하지 않습니다.
앱 source만 준비하려면 `node scripts/bootstrap.mjs --only host`를 실행합니다.

## 지도 ZIP

`pnpm bootstrap`으로 의존성과 UI를 빌드한 뒤 실행합니다.
출력 파일은 아직 존재하지 않아야 합니다. Node는 아래 경로를 현재 디렉터리 기준으로 계산합니다.

```sh
node --input-type=module -e "import fs from 'node:fs'; fs.mkdirSync('artifacts', {recursive:true})"
node --input-type=module -e "import path from 'node:path'; import {packageBundle} from './scripts/package/index.mjs'; console.log(packageBundle({source:process.cwd(),output:path.resolve('artifacts/spatial-0.1.0.zip')}))"
```

ZIP 옆에 SHA256 파일을 생성합니다. ZIP에는 `.tabularium`, JS, CSS, MapLibre worker,
의존성 라이선스와 `release.json` 파일별 검증 목록이 포함됩니다.
ZIP은 운영체제 공통이며 실제 설치 동작은 각 운영체제의 호스트가 담당합니다.

## 공식 Tabularis 0.26.0 이상에 설치

앱을 종료하고 ZIP을 임시 디렉터리에 압축 해제합니다. `.tabularium`이 있는 ZIP 루트의 모든 파일을
아래 플러그인 폴더에 복사합니다. 이전 설치본은 별도 폴더에 백업한 뒤 교체하세요.

| OS | 설치 폴더 |
| --- | --- |
| macOS | `~/Library/Application Support/tabularis/plugins/drivers/spatial/` |
| Linux | `~/.local/share/tabularis/plugins/drivers/spatial/` |
| Windows | `%APPDATA%\tabularis\plugins\drivers\spatial\` |

앱을 다시 열고 설정의 Plugins에서 `spatial`을 활성화합니다. 이 경로는 UI 전용 `driver` 패키지이며
새 PostgreSQL 드라이버 실행 파일을 설치하지 않습니다. 기존 PostgreSQL 테이블 화면의 `Map` 버튼으로
기본 지도를 엽니다. 지원 범위는 [사용 안내](usage.md)를 확인하세요.

## 공통 서비스 호스트 실행

호스트는 공간 core를 sibling 경로에서 참조합니다. 디렉터리 이름과 배치를 유지하세요.
호스트 저장소의 운영체제별 Tauri 필수 도구를 먼저 설치합니다.

```sh
pnpm --dir ../tabularis-app-source build
pnpm --dir ../tabularis-app-source tauri dev
```

배포용 앱은 `pnpm --dir ../tabularis-app-source tauri build`로 만듭니다.
코드 서명·공증 자격 증명은 이 작업에서 사용하지 않았습니다.
개발 검증에는 `TABULARIS_DATA_DIR`와 `TABULARIS_PLUGIN_DIR`에 서로 분리한 절대 경로를 지정할 수 있습니다.
두 경로는 실제 사용자 프로필과 분리해야 합니다.

수정 호스트에서도 같은 수동 설치 경로를 사용할 수 있습니다. 공통 서비스가 제공되면 전체 지도 기능을 활성화합니다.
기존 PostgreSQL 드라이버도 패치된 소스로 빌드해야 새 공간 RPC를 사용할 수 있습니다.

```sh
cargo build --locked --release --manifest-path ../tabularis-postgresql-plugin/Cargo.toml
cargo build --locked --release --manifest-path ../tabularis-sqlserver-plugin/Cargo.toml
```

PostgreSQL 드라이버 패키지는 기존 upstream과 같이 `.tabularium`과 `postgresql-plugin` 실행 파일을
ZIP 루트에 둡니다. Windows 실행 파일 이름에는 `.exe`가 붙습니다.
SQL Server는 기존 패키징 절차에 따라 `.tabularium`, 실행 파일, UI·EXPLAIN 번들을 함께 넣습니다.
SQL Server UI는 Spatial에서 `pnpm build:sdk`를 실행한 뒤 해당 저장소의 `ui/`에서
frozen install과 build를 실행합니다.

## 되돌리기

이전 호스트와 이전 드라이버 조합으로 돌아가고 새 Spatial·Cosmos 패키지를 비활성화합니다.
기존 연결과 DB 데이터를 삭제하지 않습니다.
필요하면 새 지도·결과 캐시만 별도 보관합니다. DB schema migration은 없습니다.
