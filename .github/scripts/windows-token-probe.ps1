#Requires -Version 7.0
<#
.SYNOPSIS
Verifica il token effettivo del processo e, se richiesto, l'accesso in scrittura.

.DESCRIPTION
Legge TOKEN_ELEVATION con GetTokenInformation sull'handle del processo
corrente: e' la stessa informazione che usa platform::is_elevated su Windows.
Non deduce l'elevazione dal nome del processo, dal gruppo dell'account o dal
metodo di lancio. Stampa utente, SID, bandiera del token e appartenenza al
gruppo amministratori, e termina con codice diverso da zero se il contesto non
corrisponde a quello atteso.

.PARAMETER Expect
Valore atteso di TOKEN_ELEVATION: 1 per il runner elevato, 0 per l'account di
prova non elevato.

.PARAMETER ExpectedSid
SID che il processo deve avere; vuoto per non verificarlo.

.PARAMETER WriteCheckPath
Directory in cui il processo deve poter creare e rimuovere un file di prova.
#>
[CmdletBinding()]
param(
    [ValidateSet(0, 1)]
    [int] $Expect = 0,

    [string] $ExpectedSid = '',

    [string] $WriteCheckPath = ''
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;

public static class TotumTokenProbe
{
    [StructLayout(LayoutKind.Sequential)]
    private struct TokenElevation
    {
        public int TokenIsElevated;
    }

    [DllImport("advapi32.dll", SetLastError = true)]
    private static extern bool OpenProcessToken(IntPtr process, uint access, out IntPtr token);

    [DllImport("advapi32.dll", SetLastError = true)]
    private static extern bool GetTokenInformation(
        IntPtr token,
        int informationClass,
        out TokenElevation information,
        int informationLength,
        out int returnedLength);

    [DllImport("kernel32.dll")]
    private static extern IntPtr GetCurrentProcess();

    [DllImport("kernel32.dll")]
    private static extern bool CloseHandle(IntPtr handle);

    public static int Elevation()
    {
        IntPtr token;
        if (!OpenProcessToken(GetCurrentProcess(), 0x0008, out token))
        {
            throw new System.ComponentModel.Win32Exception(Marshal.GetLastWin32Error());
        }
        try
        {
            TokenElevation information;
            int returnedLength;
            if (!GetTokenInformation(
                    token,
                    20,
                    out information,
                    Marshal.SizeOf(typeof(TokenElevation)),
                    out returnedLength))
            {
                throw new System.ComponentModel.Win32Exception(Marshal.GetLastWin32Error());
            }
            return information.TokenIsElevated;
        }
        finally
        {
            CloseHandle(token);
        }
    }
}
'@

$identity = [System.Security.Principal.WindowsIdentity]::GetCurrent()
$principal = [System.Security.Principal.WindowsPrincipal]::new($identity)
$elevation = [TotumTokenProbe]::Elevation()
$administrator = $principal.IsInRole([System.Security.Principal.WindowsBuiltInRole]::Administrator)

Write-Output "user=$($identity.Name)"
Write-Output "sid=$($identity.User.Value)"
Write-Output "tokenElevation=$elevation"
Write-Output "administratorRole=$administrator"

if ($elevation -ne $Expect) {
    Write-Error "TOKEN_ELEVATION vale $elevation invece di $Expect"
    exit 1
}

if ($ExpectedSid -and $identity.User.Value -ne $ExpectedSid) {
    Write-Error "il SID del processo non e' quello atteso"
    exit 1
}

if ($Expect -eq 0 -and $administrator) {
    Write-Error "il processo non elevato appartiene comunque al gruppo amministratori"
    exit 1
}

if ($WriteCheckPath) {
    $probeFile = Join-Path $WriteCheckPath ".totum-token-probe-$PID"
    try {
        Set-Content -LiteralPath $probeFile -Value 'ok' -NoNewline
        Remove-Item -LiteralPath $probeFile -Force
    }
    finally {
        if (Test-Path -LiteralPath $probeFile) {
            Remove-Item -LiteralPath $probeFile -Force -ErrorAction SilentlyContinue
        }
    }
    Write-Output "writeCheck=ok"
}

exit 0
