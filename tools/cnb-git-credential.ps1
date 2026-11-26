# Git credential helper for cnb.cool. Git consumes stdout privately; never run manually.
param([ValidateSet('get', 'store', 'erase')][string]$Operation = 'get')
$ErrorActionPreference = 'Stop'
if ($Operation -ne 'get') { exit 0 }

$request = @{}
while ($null -ne ($line = [Console]::In.ReadLine())) {
    if ($line.Length -eq 0) { break }
    $sep = $line.IndexOf('=')
    if ($sep -gt 0) { $request[$line.Substring(0, $sep)] = $line.Substring($sep + 1) }
}
if ($request.protocol -cne 'https' -or $request.host -cne 'cnb.cool') { exit 0 }

$tokenFile = if ($env:RHPICTURE_CNB_TOKEN_FILE) {
    $env:RHPICTURE_CNB_TOKEN_FILE
} else {
    Join-Path ([Environment]::GetFolderPath('LocalApplicationData')) 'RHFiles-Publishing\cnb-token.xml'
}
$secret = Import-Clixml -LiteralPath $tokenFile
if ($secret -isnot [Security.SecureString]) { throw 'Expected an encrypted CNB DPAPI credential' }
$ptr = [IntPtr]::Zero
try {
    $ptr = [Runtime.InteropServices.Marshal]::SecureStringToBSTR($secret)
    $val = [Runtime.InteropServices.Marshal]::PtrToStringBSTR($ptr)
    if ($val -match '[\r\n\x00]') { throw 'Invalid credential' }
    [Console]::Out.WriteLine('username=cnb')
    [Console]::Out.WriteLine('password=' + $val)
    [Console]::Out.WriteLine()
} finally {
    if ($ptr -ne [IntPtr]::Zero) { [Runtime.InteropServices.Marshal]::ZeroFreeBSTR($ptr) }
    $val = $null
    $secret.Dispose()
}
