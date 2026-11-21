# Creates a desktop shortcut to the live-interpreter window, with its seal icon.
# Run again after moving the project folder.
$ErrorActionPreference = 'Stop'
$project = Split-Path $PSScriptRoot -Parent
$exe = Join-Path $project 'target\release\live-interpreter-gui.exe'
$cli = Join-Path $project 'target\release\live-interpreter.exe'
$icon = Join-Path $project 'assets\icon.ico'
if (-not (Test-Path $exe)) {
    Write-Host "Спершу збери програму: cargo build --release" -ForegroundColor Yellow
    exit 1
}
if (-not (Test-Path $icon)) {
    & $cli icon $icon | Out-Null
}
$desktop = [Environment]::GetFolderPath('Desktop')
$link = Join-Path $desktop 'Синхронний перекладач.lnk'
# WScript.Shell saves shortcuts through the ANSI code page, so a Cyrillic file name becomes
# "?????" and Save() fails. Save under a Latin name first, then rename with PowerShell.
$temporary = Join-Path $desktop 'live-interpreter-shortcut.lnk'
$shell = New-Object -ComObject WScript.Shell
$shortcut = $shell.CreateShortcut($temporary)
$shortcut.TargetPath = $exe
$shortcut.WorkingDirectory = $project
$shortcut.IconLocation = "$icon,0"
$shortcut.Description = 'Live Interpreter: speak Ukrainian, hear Japanese'
$shortcut.Save()
Move-Item -LiteralPath $temporary -Destination $link -Force
Write-Host "Ярлик створено: $link" -ForegroundColor Green
