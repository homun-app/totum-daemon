#Requires -Version 7.0
<#
.SYNOPSIS
Esegue in CI Windows le prove di stato del daemon con un token non elevato.

.DESCRIPTION
Le prove di configurazione, identita', lock, piattaforma Windows e avvio
chiamano platform::prepare_root, che rifiuta correttamente un token elevato.
Il runner GitHub-hosted esegue come amministratore: questo harness prepara un
albero di lavoro protetto, compila le prove con l'account del runner e le
esegue con un account locale effimero che non appartiene al gruppo
amministratori. Il token effettivo viene letto prima delle prove con
GetTokenInformation(TOKEN_ELEVATION) e il SID viene confrontato con quello
dell'account creato: non si presume che il metodo di lancio riduca i privilegi.

Lo script rifiuta di funzionare fuori da un runner GitHub-hosted. Crea
l'account soltanto se non esiste gia', lo rimuove soltanto se lo ha creato e
pulisce albero e profilo anche in caso di errore. Il runner effimero resta il
limite di sicurezza: un timeout del job interrompe anche la pulizia.

Le prove del backend reale del registro sono escluse da questa esecuzione
(--skip real_backend): restano nel job dedicato, che le esegue in chiavi
isolate. Il controllo che il daemon rifiuti il contesto elevato avviene qui,
separatamente, con il token elevato del runner.
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string] $Workspace,

    [string] $WorkRoot = '',

    [int] $StepTimeoutSeconds = 600
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$script:exitCode = 0
$script:workRoot = ''
$script:workRootCreated = $false
$script:logDir = ''
$script:userName = ''
$script:userSid = ''
$script:credential = $null

function Write-Phase {
    param([Parameter(Mandatory = $true)][string] $Text)
    Write-Host ''
    Write-Host "==> $Text"
}

function Get-JsonProperty {
    param($Object, [string] $Name)
    if ($null -eq $Object) { return $null }
    $property = $Object.PSObject.Properties[$Name]
    if ($null -eq $property) { return $null }
    return $property.Value
}

function Stop-ProcessTree {
    param([Parameter(Mandatory = $true)][int] $ProcessId)
    & taskkill.exe /PID $ProcessId /T /F 2>$null | Out-Null
}

function Show-Output {
    param([Parameter(Mandatory = $true)] $Result)
    foreach ($path in @($Result.StdOut, $Result.StdErr)) {
        if ($path -and (Test-Path -LiteralPath $path)) {
            $text = Get-Content -LiteralPath $path -Raw
            if ($text) { Write-Host $text.TrimEnd() }
        }
    }
}

function Invoke-Captured {
    param(
        [Parameter(Mandatory = $true)][string] $Label,
        [Parameter(Mandatory = $true)][string] $FilePath,
        [string[]] $Arguments = @(),
        [Parameter(Mandatory = $true)][string] $WorkingDirectory,
        [int] $TimeoutSeconds = 600
    )
    $stdout = Join-Path $script:logDir "$Label.out"
    $stderr = Join-Path $script:logDir "$Label.err"
    $process = Start-Process -FilePath $FilePath -ArgumentList $Arguments -WorkingDirectory $WorkingDirectory -RedirectStandardOutput $stdout -RedirectStandardError $stderr -NoNewWindow -PassThru
    if (-not $process.WaitForExit($TimeoutSeconds * 1000)) {
        Stop-ProcessTree -ProcessId $process.Id
        $process.WaitForExit(10000) | Out-Null
        throw "timeout di $TimeoutSeconds secondi per $Label"
    }
    return [pscustomobject]@{
        ExitCode = $process.ExitCode
        StdOut   = $stdout
        StdErr   = $stderr
    }
}

function Invoke-AsUser {
    param(
        [Parameter(Mandatory = $true)][string] $Label,
        [Parameter(Mandatory = $true)][string] $FilePath,
        [string[]] $Arguments = @(),
        [int] $TimeoutSeconds = 600
    )
    $stdout = Join-Path $script:logDir "$Label.out"
    $stderr = Join-Path $script:logDir "$Label.err"
    $process = Start-Process -FilePath $FilePath -ArgumentList $Arguments -Credential $script:credential -LoadUserProfile -WorkingDirectory $script:workRoot -RedirectStandardOutput $stdout -RedirectStandardError $stderr -PassThru
    if (-not $process.WaitForExit($TimeoutSeconds * 1000)) {
        Stop-ProcessTree -ProcessId $process.Id
        $process.WaitForExit(10000) | Out-Null
        throw "timeout di $TimeoutSeconds secondi per $Label"
    }
    return [pscustomobject]@{
        ExitCode = $process.ExitCode
        StdOut   = $stdout
        StdErr   = $stderr
    }
}

function Set-TreeOwner {
    param(
        [Parameter(Mandatory = $true)][string] $Path,
        [Parameter(Mandatory = $true)][string] $Account
    )
    & icacls.exe $Path '/setowner' $Account '/T' '/C' '/Q' | Out-Null
    if ($LASTEXITCODE -ne 0) {
        throw "cambio di proprietario di $Path fallito"
    }
}

function Reset-TreeAttributes {
    param([Parameter(Mandatory = $true)][string] $Path)
    Get-ChildItem -LiteralPath $Path -Recurse -Force -ErrorAction SilentlyContinue |
        ForEach-Object { $_.Attributes = [System.IO.FileAttributes]::Normal }
}

function Remove-Tree {
    param([Parameter(Mandatory = $true)][string] $Path)
    if (-not (Test-Path -LiteralPath $Path)) { return }
    for ($attempt = 0; $attempt -lt 5; $attempt++) {
        try {
            Reset-TreeAttributes -Path $Path
            Remove-Item -LiteralPath $Path -Recurse -Force -ErrorAction Stop
            return
        }
        catch {
            Start-Sleep -Seconds 2
        }
    }
    if (Test-Path -LiteralPath $Path) {
        throw "impossibile rimuovere $Path"
    }
}

function Remove-EphemeralAccount {
    if (-not $script:userName) { return }
    $qualified = "$env:COMPUTERNAME\$($script:userName)"
    for ($attempt = 0; $attempt -lt 2; $attempt++) {
        & taskkill.exe /FI "USERNAME eq $qualified" /T /F 2>$null | Out-Null
        Start-Sleep -Seconds 2
    }
    if ($script:userSid) {
        for ($attempt = 0; $attempt -lt 3; $attempt++) {
            Get-CimInstance -ClassName Win32_UserProfile -Filter "SID='$($script:userSid)'" -ErrorAction SilentlyContinue |
                ForEach-Object { Remove-CimInstance -InputObject $_ -ErrorAction SilentlyContinue }
            Start-Sleep -Seconds 2
        }
    }
    for ($attempt = 0; $attempt -lt 5; $attempt++) {
        try {
            Remove-LocalUser -Name $script:userName -ErrorAction Stop
            break
        }
        catch {
            Start-Sleep -Seconds 2
        }
    }
    if (Get-LocalUser -Name $script:userName -ErrorAction SilentlyContinue) {
        throw "l'account effimero $($script:userName) non e' stato rimosso"
    }
    $profile = "$env:SystemDrive\Users\$($script:userName)"
    Remove-Tree -Path $profile
}

try {
    Write-Phase 'Controlli d''ambiente'
    if (-not $IsWindows) {
        throw 'lo script richiede Windows'
    }
    if ($env:GITHUB_ACTIONS -ne 'true') {
        throw 'lo script richiede GITHUB_ACTIONS=true'
    }
    if ($env:RUNNER_ENVIRONMENT -ne 'github-hosted') {
        throw 'lo script richiede RUNNER_ENVIRONMENT=github-hosted'
    }
    if (-not $env:GITHUB_RUN_ID) {
        throw 'lo script richiede un identificativo di esecuzione GitHub'
    }
    if (-not (Test-Path -LiteralPath $Workspace -PathType Container)) {
        throw "checkout non trovato: $Workspace"
    }

    $probe = Join-Path $PSScriptRoot 'windows-token-probe.ps1'
    if (-not (Test-Path -LiteralPath $probe -PathType Leaf)) {
        throw "sonda del token non trovata: $probe"
    }
    $powerShell = (Get-Process -Id $PID).Path

    Write-Phase 'Token del runner: deve essere elevato'
    & $powerShell -NoLogo -NoProfile -ExecutionPolicy Bypass -File $probe -Expect 1
    if ($LASTEXITCODE -ne 0) {
        throw 'il runner non esegue con un token elevato: la prova di rifiuto non sarebbe significativa'
    }

    if (-not $WorkRoot) {
        $suffixParts = @($env:GITHUB_RUN_ID)
        if ($env:GITHUB_RUN_ATTEMPT) { $suffixParts += $env:GITHUB_RUN_ATTEMPT }
        $WorkRoot = "$env:SystemDrive\totum-ci-" + ($suffixParts -join '-')
    }
    $script:workRoot = [System.IO.Path]::GetFullPath($WorkRoot)
    if (-not $script:workRoot.StartsWith($env:SystemDrive + '\', [System.StringComparison]::OrdinalIgnoreCase)) {
        throw "l'albero di lavoro deve stare sul volume di sistema: $($script:workRoot)"
    }
    if (Test-Path -LiteralPath $script:workRoot) {
        throw "l'albero di lavoro esiste gia' e non viene riusato: $($script:workRoot)"
    }

    Write-Phase 'Albero di lavoro protetto'
    $script:workRootCreated = $true
    New-Item -ItemType Directory -Path $script:workRoot | Out-Null
    $script:logDir = Join-Path $script:workRoot 'logs'
    New-Item -ItemType Directory -Path $script:logDir | Out-Null
    & icacls.exe $script:workRoot '/inheritance:r' | Out-Null
    if ($LASTEXITCODE -ne 0) { throw 'rimozione dell''ereditarieta'' fallita' }
    & icacls.exe $script:workRoot '/grant:r' '*S-1-5-18:(OI)(CI)F' '*S-1-5-32-544:(OI)(CI)F' | Out-Null
    if ($LASTEXITCODE -ne 0) { throw 'concessione delle protezioni di base fallita' }

    Write-Phase 'Copia del checkout nell''albero protetto'
    $source = Join-Path $script:workRoot 'src'
    $robocopy = (Get-Command robocopy.exe -ErrorAction Stop).Source
    $copy = Start-Process -FilePath $robocopy -ArgumentList @(
        $Workspace, $source, '/E', '/XD', '.git', 'target',
        '/NFL', '/NDL', '/NJH', '/NJS', '/NP', '/R:2', '/W:2'
    ) -Wait -PassThru -NoNewWindow
    if ($copy.ExitCode -ge 8) {
        throw "copia del checkout fallita con codice $($copy.ExitCode)"
    }

    Write-Phase 'Compilazione delle prove (account del runner)'
    $cargo = (Get-Command cargo -ErrorAction Stop).Source
    $compile = Invoke-Captured -Label 'compile' -FilePath $cargo -Arguments @(
        'test', '-p', 'totum-daemon',
        '--lib',
        '--test', 'config',
        '--test', 'identity',
        '--test', 'lock',
        '--test', 'platform_windows',
        '--test', 'startup',
        '--no-run', '--locked', '--message-format=json'
    ) -WorkingDirectory $source -TimeoutSeconds 900
    if ($compile.ExitCode -ne 0) {
        Show-Output -Result $compile
        throw 'compilazione delle prove fallita'
    }

    $wanted = @('totum_daemon', 'config', 'identity', 'lock', 'platform_windows', 'startup')
    $testArtifacts = @{}
    foreach ($line in Get-Content -LiteralPath $compile.StdOut) {
        if ([string]::IsNullOrWhiteSpace($line)) { continue }
        try { $item = $line | ConvertFrom-Json -ErrorAction Stop }
        catch { continue }
        if ((Get-JsonProperty $item 'reason') -ne 'compiler-artifact') { continue }
        $profile = Get-JsonProperty $item 'profile'
        if ((Get-JsonProperty $profile 'test') -ne $true) { continue }
        $executable = Get-JsonProperty $item 'executable'
        if (-not $executable) { continue }
        $target = Get-JsonProperty $item 'target'
        $targetName = Get-JsonProperty $target 'name'
        if (-not ($wanted -contains $targetName)) { continue }
        $kind = @(Get-JsonProperty $target 'kind')
        if ($targetName -eq 'totum_daemon') {
            if (-not ($kind -contains 'lib')) { continue }
        }
        elseif (-not ($kind -contains 'test')) {
            continue
        }
        $testArtifacts[$targetName] = $executable
    }
    foreach ($name in $wanted) {
        if (-not $testArtifacts.ContainsKey($name)) {
            throw "la prova $name non e' stata compilata"
        }
    }

    $build = Invoke-Captured -Label 'build-daemon' -FilePath $cargo -Arguments @(
        'build', '-p', 'totum-daemon', '--bin', 'totum-daemon', '--locked'
    ) -WorkingDirectory $source -TimeoutSeconds 900
    if ($build.ExitCode -ne 0) {
        Show-Output -Result $build
        throw 'compilazione del binario del daemon fallita'
    }
    $daemon = Join-Path $source 'target\debug\totum-daemon.exe'
    if (-not (Test-Path -LiteralPath $daemon -PathType Leaf)) {
        throw "binario del daemon non trovato: $daemon"
    }

    Write-Phase 'Il daemon rifiuta il contesto elevato (token del runner)'
    $elevatedAppData = Join-Path $script:workRoot 'elevated-localappdata'
    $savedLocalAppData = $env:LOCALAPPDATA
    $env:LOCALAPPDATA = $elevatedAppData
    try {
        $refusal = Invoke-Captured -Label 'elevated-refusal' -FilePath $daemon -Arguments @() -WorkingDirectory $source -TimeoutSeconds 60
    }
    finally {
        if ($null -eq $savedLocalAppData) {
            Remove-Item Env:LOCALAPPDATA -ErrorAction SilentlyContinue
        }
        else {
            $env:LOCALAPPDATA = $savedLocalAppData
        }
    }
    Show-Output -Result $refusal
    if ($refusal.ExitCode -ne 1) {
        throw "il daemon elevato e' uscito con $($refusal.ExitCode) invece di 1"
    }
    $refusalText = Get-Content -LiteralPath $refusal.StdErr -Raw
    if ($refusalText -notmatch 'esecuzione elevata rifiutata') {
        throw 'il daemon elevato non ha diagnosticato il rifiuto dell''elevazione'
    }
    if (Test-Path -LiteralPath (Join-Path $elevatedAppData 'Totum')) {
        throw 'il rifiuto dell''elevazione ha comunque creato la radice di stato'
    }
    Write-Host 'rifiuto verificato: uscita 1, diagnosi presente, nessuna radice creata'

    Write-Phase 'Account effimero non amministratore'
    $digits = $env:GITHUB_RUN_ID -replace '[^0-9]', ''
    $name = 'tw' + $digits + (Get-Random -Minimum 1000 -Maximum 9999).ToString()
    if ($name.Length -gt 20) { $name = $name.Substring(0, 20) }
    if (Get-LocalUser -Name $name -ErrorAction SilentlyContinue) {
        throw "l'account $name esiste gia' e non viene toccato"
    }
    $password = [Guid]::NewGuid().ToString('N') + '!Aa1'
    $secure = ConvertTo-SecureString -String $password -AsPlainText -Force
    $account = New-LocalUser -Name $name -Password $secure -PasswordNeverExpires -AccountNeverExpires -Description 'Totum CI ephemeral non-elevated account'
    $script:userName = $name
    $script:userSid = $account.SID.Value
    $users = Get-LocalGroup -SID 'S-1-5-32-545'
    $members = @(Get-LocalGroupMember -Group $users -ErrorAction SilentlyContinue)
    if (-not ($members | Where-Object { $_.SID.Value -eq $script:userSid })) {
        Add-LocalGroupMember -Group $users -Member $name
    }
    $script:credential = [System.Management.Automation.PSCredential]::new("$env:COMPUTERNAME\$name", $secure)
    Write-Host "account creato: $name (SID $($script:userSid)), non amministratore"

    Write-Phase 'Accesso dell''account all''albero di prova'
    & icacls.exe $script:workRoot '/grant' "${name}:(OI)(CI)F" '/T' '/Q' | Out-Null
    if ($LASTEXITCODE -ne 0) { throw 'concessione dell''accesso all''albero di prova fallita' }

    # platform::prepare_root pretende che ogni antenato esistente sia posseduto
    # dall'utente corrente o da un principale di sistema: l'albero e' stato
    # creato dal runner amministratore, quindi va ceduto all'account di prova.
    Write-Phase 'Proprietario dell''albero di prova'
    $qualified = "$env:COMPUTERNAME\$name"
    Set-TreeOwner -Path $script:workRoot -Account $qualified
    $owner = (Get-Acl -LiteralPath $source).Owner
    if ($owner -ine $qualified) {
        throw "il proprietario di $source e' $owner invece di $qualified"
    }
    $targetOwner = (Get-Acl -LiteralPath (Join-Path $source 'target')).Owner
    if ($targetOwner -ine $qualified) {
        throw "il proprietario di target e' $targetOwner invece di $qualified"
    }

    Write-Phase 'Token effettivo dell''account di prova'
    $probeResult = Invoke-AsUser -Label 'token-probe' -FilePath $powerShell -Arguments @(
        '-NoLogo', '-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', $probe,
        '-Expect', '0', '-ExpectedSid', $script:userSid,
        '-WriteCheckPath', (Join-Path $source 'target')
    ) -TimeoutSeconds 120
    Show-Output -Result $probeResult
    if ($probeResult.ExitCode -ne 0) {
        throw 'il token dell''account di prova non e'' non elevato o non corrisponde al SID creato'
    }

    Write-Phase 'Prove di stato con token non elevato'
    $failures = [System.Collections.Generic.List[string]]::new()
    foreach ($suite in @('config', 'identity', 'lock', 'platform_windows', 'startup', 'totum_daemon')) {
        Write-Phase "Esecuzione $suite"
        $result = Invoke-AsUser -Label "test-$suite" -FilePath $testArtifacts[$suite] -Arguments @('--skip', 'real_backend') -TimeoutSeconds $StepTimeoutSeconds
        Show-Output -Result $result
        if ($result.ExitCode -ne 0) {
            $failures.Add($suite)
            Write-Host "FALLITE: $suite (uscita $($result.ExitCode))" -ForegroundColor Red
        }
        else {
            Write-Host "SUPERATE: $suite"
        }
    }
    if ($failures.Count -gt 0) {
        throw "prove fallite: $($failures -join ', ')"
    }
}
catch {
    $script:exitCode = 1
    Write-Host ''
    Write-Host "ERRORE: $($_.Exception.Message)" -ForegroundColor Red
    Write-Host $_.ScriptStackTrace
}
finally {
    Write-Phase 'Pulizia'
    try {
        Remove-EphemeralAccount
    }
    catch {
        $script:exitCode = 1
        Write-Host "ERRORE DI PULIZIA: $($_.Exception.Message)" -ForegroundColor Red
    }
    if ($script:workRootCreated) {
        try {
            Remove-Tree -Path $script:workRoot
        }
        catch {
            $script:exitCode = 1
            Write-Host "ERRORE DI PULIZIA: $($_.Exception.Message)" -ForegroundColor Red
        }
    }
}

exit $script:exitCode
