<#
.SYNOPSIS
    Upload packaged RHPicture Velopack release artifacts (including delta packages & releases.win.json)
    to GitHub Releases and/or CNB (cnb.cool) Releases. Credentials are never printed or passed in argv.

.EXAMPLE
    pwsh -File tools\publish-release.ps1 -Provider github -Repository RailgunHamster/RHPicture -Version 0.1.0 -NotesFile RELEASE-NOTES-v0.1.0.md

.EXAMPLE
    pwsh -File tools\publish-release.ps1 -Provider cnb -Repository railgunhamster/RHPicture -Version 0.1.0 -NotesFile RELEASE-NOTES-v0.1.0.md -BundlePath rhpicture.bundle
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory)][ValidateSet('cnb', 'github')][string]$Provider,
    [Parameter(Mandatory)][string]$Repository,
    [Parameter(Mandatory)][ValidatePattern('^\d+\.\d+\.\d+(?:-[A-Za-z0-9.]+)?$')][string]$Version,
    [string]$ReleaseDir = '',
    [string]$NotesFile = '',
    [string]$BundlePath = '',
    [string]$CnbTokenFile = "$env:LOCALAPPDATA\RHFiles-Publishing\cnb-token.xml"
)

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12

$repoRoot = Split-Path $PSScriptRoot -Parent
$dir = if ($ReleaseDir) { [IO.Path]::GetFullPath($ReleaseDir) } else { Join-Path $repoRoot 'dist\releases' }
$tag = "v$Version"

if (-not (Test-Path -LiteralPath (Join-Path $dir 'releases.win.json'))) {
    throw "releases.win.json not found in $dir. Run tools\package-windows-velopack.ps1 first."
}

$notes = if ($NotesFile -and (Test-Path -LiteralPath $NotesFile)) {
    [IO.File]::ReadAllText($NotesFile, [Text.UTF8Encoding]::new($false))
} else {
    "RHPicture $tag"
}

$files = @(
    'releases.win.json',
    'RELEASES',
    "RHPicture-$Version-full.nupkg",
    "RHPicture-$Version-delta.nupkg",
    'RHPicture-win-Setup.exe',
    'RHPicture-win-Portable.zip'
) | ForEach-Object { Join-Path $dir $_ } | Where-Object { Test-Path -LiteralPath $_ }

if ($files.Count -eq 0) {
    throw "No release artifacts found in $dir for version $Version"
}

function Get-GitCredentialPassword([string]$HostName) {
    $psi = [Diagnostics.ProcessStartInfo]::new('git', 'credential fill')
    $psi.UseShellExecute = $false
    $psi.CreateNoWindow = $true
    $psi.RedirectStandardInput = $true
    $psi.RedirectStandardOutput = $true
    $psi.RedirectStandardError = $true
    $psi.Environment['GIT_TERMINAL_PROMPT'] = '0'
    $psi.Environment['GCM_INTERACTIVE'] = 'never'
    $proc = [Diagnostics.Process]::Start($psi)
    try {
        $stdout = $proc.StandardOutput.ReadToEndAsync()
        $stderr = $proc.StandardError.ReadToEndAsync()
        $proc.StandardInput.Write("protocol=https`nhost=$HostName`n`n")
        $proc.StandardInput.Close()
        if (-not $proc.WaitForExit(10000)) {
            $proc.Kill()
            throw "git credential lookup timed out for $HostName"
        }
        $fields = @{}
        foreach ($line in ($stdout.Result -split "`r?`n")) {
            if ($line -match '^([^=]+)=(.*)$') { $fields[$Matches[1]] = $Matches[2] }
        }
        return $fields.password
    } finally {
        $proc.Dispose()
    }
}

Add-Type -AssemblyName System.Net.Http

if ($Provider -eq 'github') {
    $token = $env:GITHUB_TOKEN
    if (-not $token) { $token = Get-GitCredentialPassword 'github.com' }
    if (-not $token) { throw 'GitHub credential unavailable (set GITHUB_TOKEN or sign in via Git Credential Manager).' }

    $apiBase = "https://api.github.com/repos/$Repository"
    $headers = @{
        Authorization = "Bearer $token"
        'User-Agent'  = 'RHPicture-Publisher'
        Accept        = 'application/vnd.github+json'
    }

    function Invoke-GitHubApi([string]$Method, [string]$Path, $Body = $null, [switch]$Allow404) {
        try {
            $params = @{
                Uri                = "$apiBase$Path"
                Method             = $Method
                Headers            = $headers
                TimeoutSec         = 30
                MaximumRedirection = 0
            }
            if ($null -ne $Body) {
                $params.ContentType = 'application/json; charset=utf-8'
                $params.Body = [Text.Encoding]::UTF8.GetBytes(($Body | ConvertTo-Json -Depth 8 -Compress))
            }
            Invoke-RestMethod @params
        } catch {
            $code = if ($_.Exception.Response) { [int]$_.Exception.Response.StatusCode } else { 0 }
            if ($Allow404 -and $code -eq 404) { return $null }
            throw "GitHub API $Method $Path failed (HTTP $code; credentials suppressed)"
        }
    }

    $client = [Net.Http.HttpClient]::new()
    $client.Timeout = [TimeSpan]::FromMinutes(15)
    $client.DefaultRequestHeaders.Authorization = [Net.Http.Headers.AuthenticationHeaderValue]::new('Bearer', $token)
    $client.DefaultRequestHeaders.UserAgent.ParseAdd('RHPicture-Publisher')
    try {
        $release = Invoke-GitHubApi 'GET' "/releases/tags/$tag" $null -Allow404
        if (-not $release) {
            # Also check draft releases so we reuse an existing draft for this tag
            $all = Invoke-GitHubApi 'GET' '/releases?per_page=30'
            $release = @($all | Where-Object { $_.tag_name -ceq $tag }) | Select-Object -First 1
        }
        if (-not $release) {
            $release = Invoke-GitHubApi 'POST' '/releases' @{
                tag_name         = $tag
                target_commitish = 'main'
                name             = "RHPicture $tag"
                body             = $notes
                draft            = $true
                prerelease       = $false
            }
        }
        $uploadBase = ([string]$release.upload_url) -replace '\{.*$', ''
        if (-not $uploadBase.StartsWith('https://uploads.github.com/')) {
            throw 'Unexpected GitHub upload_url'
        }
        foreach ($filePath in $files) {
            $fi = Get-Item -LiteralPath $filePath
            $existing = @($release.assets | Where-Object { $_.name -ceq $fi.Name })
            if ($existing.Count -gt 0) {
                if ($existing[0].size -eq $fi.Length) {
                    Write-Output "GitHub already has $($fi.Name) ($($fi.Length) bytes)"
                    continue
                }
                $null = Invoke-GitHubApi 'DELETE' "/releases/assets/$($existing[0].id)"
            }
            $stream = [IO.File]::OpenRead($fi.FullName)
            $content = $null
            try {
                $content = [Net.Http.StreamContent]::new($stream)
                $content.Headers.ContentType = [Net.Http.Headers.MediaTypeHeaderValue]::new('application/octet-stream')
                $uri = "${uploadBase}?name=$([Uri]::EscapeDataString($fi.Name))"
                $resp = $client.PostAsync($uri, $content).GetAwaiter().GetResult()
                if (-not $resp.IsSuccessStatusCode) {
                    $st = [int]$resp.StatusCode
                    $resp.Dispose()
                    throw "GitHub upload $($fi.Name) failed (HTTP $st)"
                }
                $resp.Dispose()
                Write-Output "Uploaded to GitHub: $($fi.Name) ($($fi.Length) bytes)"
            } finally {
                if ($content) { $content.Dispose() }
                $stream.Dispose()
            }
        }
        $published = Invoke-GitHubApi 'PATCH' "/releases/$($release.id)" @{
            draft       = $false
            prerelease  = $false
            make_latest = 'true'
            name        = "RHPicture $tag"
            body        = $notes
        }
        Write-Output "GitHub release published: $($published.html_url)"
    } finally {
        $client.Dispose()
        $token = $null
        $headers = $null
    }
    return
}

# CNB publication (adapted from D:\git\RHRemote\tools\cnb-release.ps1)
$token = $env:CNB_TOKEN
if (-not $token -and (Test-Path -LiteralPath $CnbTokenFile)) {
    $sec = Import-Clixml -LiteralPath $CnbTokenFile
    if ($sec -is [Security.SecureString]) {
        $token = [Net.NetworkCredential]::new('', $sec).Password
    }
}
if (-not $token) {
    $token = Get-GitCredentialPassword 'cnb.cool'
}
if (-not $token) {
    throw "CNB credential unavailable. Set `$env:CNB_TOKEN or provide DPAPI token file at $CnbTokenFile."
}

# Optionally push Git source (main + tag) from a bundle using the DPAPI credential helper
if ($BundlePath) {
    if (-not (Test-Path -LiteralPath $BundlePath -PathType Leaf)) {
        throw "Git bundle not found: $BundlePath"
    }
    $bare = Join-Path ([IO.Path]::GetTempPath()) ('rhpicture-cnb-git-' + [guid]::NewGuid().ToString('N') + '.git')
    try {
        $null = & git init --bare $bare 2>$null
        if ($LASTEXITCODE -ne 0) { throw 'Failed to init bare git repo' }
        $null = & git -C $bare fetch $BundlePath 'refs/heads/main:refs/heads/main' ("refs/tags/${tag}:refs/tags/${tag}") 2>$null
        if ($LASTEXITCODE -ne 0) { throw 'Failed to fetch from git bundle' }
        $helper = (Join-Path $PSScriptRoot 'cnb-git-credential.ps1').Replace('\', '/')
        $pwsh = (Get-Process -Id $PID).Path.Replace('\', '/')
        $credCmd = '!"' + $pwsh + '" -NoProfile -NonInteractive -File "' + $helper + '"'
        $psi = [Diagnostics.ProcessStartInfo]::new('git')
        $psi.UseShellExecute = $false
        $psi.CreateNoWindow = $true
        $psi.RedirectStandardOutput = $true
        $psi.RedirectStandardError = $true
        foreach ($arg in @('-C', $bare, '-c', 'http.proxy=', '-c', 'https.proxy=', '-c', 'credential.helper=', '-c', ("credential.helper=$credCmd"), 'push', "https://cnb.cool/$Repository.git", 'refs/heads/main:refs/heads/main', "refs/tags/${tag}:refs/tags/${tag}")) {
            $psi.ArgumentList.Add($arg)
        }
        $psi.Environment['GIT_TERMINAL_PROMPT'] = '0'
        $psi.Environment['RHPICTURE_CNB_TOKEN_FILE'] = [IO.Path]::GetFullPath($CnbTokenFile)
        $gproc = [Diagnostics.Process]::Start($psi)
        try {
            $null = $gproc.StandardOutput.ReadToEndAsync()
            $null = $gproc.StandardError.ReadToEndAsync()
            if (-not $gproc.WaitForExit(180000)) { $gproc.Kill(); throw 'CNB git push timed out' }
            if ($gproc.ExitCode -ne 0) { throw "CNB git push failed (exit $($gproc.ExitCode))" }
        } finally {
            $gproc.Dispose()
        }
        Write-Output "Pushed source main + $tag to https://cnb.cool/$Repository.git"
    } finally {
        if (Test-Path -LiteralPath $bare) { Remove-Item -LiteralPath $bare -Recurse -Force -ErrorAction SilentlyContinue }
    }
}

$handler = [Net.Http.HttpClientHandler]::new()
$handler.AllowAutoRedirect = $false
$handler.UseProxy = $false
$client = [Net.Http.HttpClient]::new($handler)
$client.Timeout = [TimeSpan]::FromMinutes(15)
$client.DefaultRequestHeaders.UserAgent.ParseAdd('RHPicture-Publisher')

function Invoke-CnbApi([string]$Path, [string]$Method = 'GET', $Body = $null, [switch]$Allow404) {
    $uri = [uri]::new([uri]'https://api.cnb.cool/', $Path)
    $req = [Net.Http.HttpRequestMessage]::new([Net.Http.HttpMethod]::new($Method), $uri)
    $req.Headers.Authorization = [Net.Http.Headers.AuthenticationHeaderValue]::new('Bearer', $token)
    $req.Headers.Accept.ParseAdd('application/vnd.cnb.api+json')
    if ($null -ne $Body) {
        $req.Content = [Net.Http.StringContent]::new(($Body | ConvertTo-Json -Depth 10 -Compress), [Text.Encoding]::UTF8, 'application/json')
    }
    $resp = $client.SendAsync($req).GetAwaiter().GetResult()
    try {
        if ([int]$resp.StatusCode -eq 404 -and $Allow404) { return $null }
        if (-not $resp.IsSuccessStatusCode) {
            throw "CNB API $Method $Path failed (HTTP $([int]$resp.StatusCode))"
        }
        $txt = $resp.Content.ReadAsStringAsync().GetAwaiter().GetResult()
        if ($txt.Trim()) { return $txt | ConvertFrom-Json }
    } finally {
        $resp.Dispose()
        $req.Dispose()
    }
}

try {
    $release = Invoke-CnbApi "/$Repository/-/releases/tags/$tag" 'GET' $null -Allow404
    if (-not $release) {
        $release = Invoke-CnbApi "/$Repository/-/releases" 'POST' @{
            tag_name    = $tag
            name        = "RHPicture $tag"
            body        = $notes
            draft       = $false
            prerelease  = $true
            make_latest = 'false'
        }
    }

    foreach ($filePath in $files) {
        $fi = Get-Item -LiteralPath $filePath
        $upload = Invoke-CnbApi "/$Repository/-/releases/$($release.id)/asset-upload-url" 'POST' @{
            asset_name = $fi.Name
            size       = $fi.Length
            overwrite  = $true
            ttl        = 0
        }
        $putReq = [Net.Http.HttpRequestMessage]::new([Net.Http.HttpMethod]::Put, [uri]$upload.upload_url)
        $stream = [IO.File]::OpenRead($fi.FullName)
        try {
            $putReq.Content = [Net.Http.StreamContent]::new($stream)
            $putReq.Content.Headers.ContentType = [Net.Http.Headers.MediaTypeHeaderValue]::new('application/octet-stream')
            $putReq.Content.Headers.ContentLength = $fi.Length
            $putResp = $client.SendAsync($putReq).GetAwaiter().GetResult()
            if (-not $putResp.IsSuccessStatusCode) {
                throw "CNB upload PUT failed for $($fi.Name) (HTTP $([int]$putResp.StatusCode))"
            }
            $putResp.Dispose()
        } finally {
            $stream.Dispose()
            $putReq.Dispose()
        }
        $verifyPath = ([uri]::new([uri]'https://api.cnb.cool/', $upload.verify_url)).PathAndQuery
        $null = Invoke-CnbApi $verifyPath 'POST'
        Write-Output "Uploaded to CNB: $($fi.Name) ($($fi.Length) bytes)"
    }

    $null = Invoke-CnbApi "/$Repository/-/releases/$($release.id)" 'PATCH' @{
        draft       = $false
        prerelease  = $false
        make_latest = 'true'
        name        = "RHPicture $tag"
        body        = $notes
    }
    Write-Output "CNB release $tag published: https://cnb.cool/$Repository/-/releases/tag/$tag"
} finally {
    $client.Dispose()
    $token = $null
}
