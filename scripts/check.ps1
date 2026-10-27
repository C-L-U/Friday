$ErrorActionPreference = 'Stop'
foreach ($step in @('check', 'clippy', 'test')) {
    if ($step -eq 'clippy') { & cargo clippy -- -D warnings } else { & cargo $step }
    if ($LASTEXITCODE -ne 0) { throw "cargo $step failed ($LASTEXITCODE)" }
}
