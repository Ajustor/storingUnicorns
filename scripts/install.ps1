# Install storingUnicorns on Windows (per user, no admin rights):
#
#   irm https://ajustor.github.io/storingUnicorns/install.ps1 | iex
#
# Installs into %LOCALAPPDATA%\Programs\storingUnicorns, adds it to the user PATH
# and creates a Start menu shortcut. The download is checked against the
# SHA-256 published in latest.json. The app then updates itself in place.
$ErrorActionPreference = 'Stop'

$base = 'https://ajustor.github.io/storingUnicorns'
$assetName = 'storingUnicorns-windows-x64.exe'
$dir = Join-Path $env:LOCALAPPDATA 'Programs\storingUnicorns'
$exe = Join-Path $dir 'storingUnicorns.exe'

$manifest = Invoke-RestMethod "$base/latest.json"
$asset = $manifest.assets | Where-Object name -eq $assetName
if (-not $asset) { throw "Aucun binaire $assetName dans la version $($manifest.version)." }

New-Item -ItemType Directory -Force -Path $dir | Out-Null
$tmp = Join-Path ([IO.Path]::GetTempPath()) $assetName
Write-Host "Téléchargement de storingUnicorns v$($manifest.version)…"
Invoke-WebRequest $asset.url -OutFile $tmp -UseBasicParsing

$actual = (Get-FileHash $tmp -Algorithm SHA256).Hash
if ($actual -ne $asset.sha256.ToUpper()) {
    Remove-Item $tmp -Force
    throw "Empreinte SHA-256 incorrecte (attendu $($asset.sha256), obtenu $actual)."
}
Move-Item $tmp $exe -Force

$userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
if (($userPath -split ';') -notcontains $dir) {
    [Environment]::SetEnvironmentVariable('Path', ($userPath.TrimEnd(';') + ";$dir"), 'User')
    Write-Host "$dir ajouté au PATH (ouvrez un nouveau terminal)."
}

$startMenu = Join-Path $env:APPDATA 'Microsoft\Windows\Start Menu\Programs\storingUnicorns.lnk'
$shell = New-Object -ComObject WScript.Shell
$link = $shell.CreateShortcut($startMenu)
$link.TargetPath = $exe
$link.WorkingDirectory = $dir
$link.Save()

Write-Host "storingUnicorns v$($manifest.version) installé. Lancez-le depuis le menu Démarrer ou avec : storingUnicorns"
