$ErrorActionPreference = 'Stop'
$messages = Join-Path $env:RUNNER_TEMP 'spatial-install-cargo.jsonl'
& cargo test --locked --manifest-path ../tabularis-app-source/src-tauri/Cargo.toml --lib --no-run --message-format=json |
    Tee-Object -FilePath $messages | Out-Null
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

$executables = @(Get-Content $messages | ForEach-Object { $_ | ConvertFrom-Json } |
    Where-Object { $_.reason -eq 'compiler-artifact' -and $_.target.name -eq 'tabularis_lib' -and $_.profile.test -and $_.executable } |
    Select-Object -ExpandProperty executable)
if ($executables.Count -ne 1) { throw 'Expected exactly one native host test executable.' }
$executable = $executables[0]
$mt = Get-ChildItem "${env:ProgramFiles(x86)}\Windows Kits\10\bin\*\x64\mt.exe" |
    Sort-Object FullName -Descending | Select-Object -First 1
if (-not $mt) { throw 'Windows SDK manifest tool is unavailable.' }

$before = Join-Path $env:RUNNER_TEMP 'spatial-test-before.manifest'
& $mt.FullName -nologo "-inputresource:$executable;#1" "-out:$before"
if ($LASTEXITCODE -eq 0) { Get-Content $before }

$manifest = Join-Path $PSScriptRoot 'windows-test.manifest'
& $mt.FullName -nologo -manifest $manifest "-outputresource:$executable;#1"
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

& $executable 'plugins::native_package_tests::현재_os의_실제_설치기는_지도_zip과_로컬_자산을_그대로_설치한다' --ignored --exact --test-threads=1
exit $LASTEXITCODE
