#Requires -Version 7.0
# Prove non privilegiate delle funzioni di cattura: nessun account o ACL reale.
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$tokens = $null
$errors = $null
$harness = Join-Path $PSScriptRoot 'windows-non-elevated.ps1'
$ast = [System.Management.Automation.Language.Parser]::ParseFile($harness, [ref]$tokens, [ref]$errors)
if ($errors.Count -gt 0) { throw ($errors | Out-String) }
foreach ($definition in $ast.FindAll({ param($node) $node -is [System.Management.Automation.Language.FunctionDefinitionAst] }, $false)) {
    . ([scriptblock]::Create($definition.Extent.Text))
}
if (-not $IsWindows) {
    # La terminazione Windows resta verificata soltanto sul runner Windows.
    function Stop-ProcessTree {
        param([int] $ProcessId)
        $child = Get-Process -Id $ProcessId -ErrorAction SilentlyContinue
        if ($child) { $child.Kill($true); $child.WaitForExit(10000) | Out-Null }
    }
}

$script:logDir = Join-Path ([System.IO.Path]::GetTempPath()) ('totum-capture-selftest-' + [Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $script:logDir | Out-Null
$powerShell = (Get-Process -Id $PID).Path
$fixture = Join-Path $script:logDir 'fixture.ps1'
try {
    Set-Content -LiteralPath $fixture -Value @'
[Console]::Out.WriteLine('synthetic-stdout')
[Console]::Error.WriteLine('synthetic-stderr')
Start-Sleep -Seconds 30
'@
    $script:timedOut = $false
    $diagnostic = & {
        try {
            $null = Invoke-Captured -Label 'timeout' -FilePath $powerShell -Arguments @('-NoLogo', '-NoProfile', '-File', $fixture) -WorkingDirectory $script:logDir -TimeoutSeconds 2
        }
        catch {
            $script:timeoutMessage = $_.Exception.Message
            $script:timedOut = $true
        }
    } *>&1 | Out-String
    if (-not $script:timedOut -or $script:timeoutMessage -notmatch 'timeout') { throw 'timeout non segnalato' }
    if ($diagnostic -notmatch 'synthetic-stdout' -or $diagnostic -notmatch 'synthetic-stderr') {
        throw 'il timeout ha perso stdout/stderr del processo figlio'
    }
    if ($diagnostic -notmatch 'terminato=True') { throw 'il figlio del timeout non risulta terminato' }
    Write-Host 'PASS: timeout con stdout/stderr conservati'

    Set-Content -LiteralPath $fixture -Value 'exit 7'
    $result = Invoke-Captured -Label 'nonzero' -FilePath $powerShell -Arguments @('-NoLogo', '-NoProfile', '-File', $fixture) -WorkingDirectory $script:logDir -TimeoutSeconds 10
    if ($result.ExitCode -ne 7) { throw "codice di uscita perso: $($result.ExitCode)" }
    Write-Host 'PASS: uscita non zero conservata'

    # Solo il confine privilegiato e' simulato: il figlio rimuove davvero
    # l'albero sintetico, con spazi e apostrofi, usando il normale token locale.
    function taskkill.exe {
        $script:stopRequested = $true
        $global:LASTEXITCODE = 0
    }
    function Invoke-AsUser {
        param($Label, $FilePath, [string[]] $Arguments, $TimeoutSeconds)
        if (-not $script:stopRequested) { throw 'pulizia avviata prima dello stop' }
        return Invoke-Captured -Label $Label -FilePath $FilePath -Arguments $Arguments -WorkingDirectory $script:workRoot -TimeoutSeconds $TimeoutSeconds
    }
    $script:workRoot = Join-Path $script:logDir "owner's work tree"
    $script:userName = 'synthetic-account'
    $script:credential = [pscustomobject]@{ Synthetic = $true }
    $script:workRootCreated = $false
    $script:stopRequested = $false
    New-Item -ItemType Directory -Path (Join-Path $script:workRoot 'src/state') | Out-Null
    Set-Content -LiteralPath (Join-Path $script:workRoot 'src/state/config.json') -Value '{}'
    Remove-OwnedTestSource
    if ($script:stopRequested) { throw 'pulizia senza albero creato dall''harness' }
    $script:workRootCreated = $true
    Remove-OwnedTestSource
    if (Test-Path -LiteralPath (Join-Path $script:workRoot 'src')) { throw 'sorgente sintetica ancora presente' }
    if (-not (Test-Path -LiteralPath $fixture)) { throw 'pulizia fuori dal sottoalbero src' }
    Write-Host 'PASS: pulizia limitata alla sorgente dopo lo stop dei processi'
}
finally {
    Remove-Item -LiteralPath $script:logDir -Recurse -Force
}
