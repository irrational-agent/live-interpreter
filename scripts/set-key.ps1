# Prompts for an API key and stores it in .env.local under the given variable name.
# The key is read as a SecureString, so it is never echoed to the screen or written to any log.
#
#   set-key.ps1                                   # ELEVENLABS_API_KEY
#   set-key.ps1 -Name ANTHROPIC_API_KEY           # Claude API key
param(
    [ValidateSet('ELEVENLABS_API_KEY', 'ANTHROPIC_API_KEY')]
    [string]$Name = 'ELEVENLABS_API_KEY'
)
$ErrorActionPreference = 'Stop'

$presets = @{
    ELEVENLABS_API_KEY = @{ Title = 'ElevenLabs API key'; Url = 'https://elevenlabs.io/app/settings/api-keys'; Prefix = 'sk_' }
    ANTHROPIC_API_KEY  = @{ Title = 'Claude (Anthropic) API key'; Url = 'https://platform.claude.com/settings/keys'; Prefix = 'sk-ant-' }
}
$preset = $presets[$Name]
$envFile = Join-Path (Split-Path $PSScriptRoot -Parent) '.env.local'

Write-Host ''
Write-Host "$($preset.Title) -> .env.local ($Name)" -ForegroundColor Cyan
Write-Host "Get a key at $($preset.Url)"
Write-Host 'Paste it below (right-click or Ctrl+V). Nothing will be shown while you paste.'
Write-Host ''

$secure = Read-Host -AsSecureString 'API key'
$bstr = [Runtime.InteropServices.Marshal]::SecureStringToBSTR($secure)
try {
    $key = [Runtime.InteropServices.Marshal]::PtrToStringBSTR($bstr).Trim()
} finally {
    [Runtime.InteropServices.Marshal]::ZeroFreeBSTR($bstr)
}

if ([string]::IsNullOrWhiteSpace($key)) {
    Write-Host 'No key entered, nothing saved.' -ForegroundColor Yellow
    exit 1
}
if ($key -match '\s') {
    Write-Host 'The pasted text contains spaces or line breaks; that does not look like an API key. Nothing saved.' -ForegroundColor Yellow
    exit 1
}
if (-not $key.StartsWith($preset.Prefix)) {
    Write-Host "A $($preset.Title) starts with '$($preset.Prefix)'; this one does not. Wrong key copied? Nothing saved." -ForegroundColor Yellow
    exit 1
}

# Keep any other lines already in the file, replace only this variable.
$lines = @()
if (Test-Path $envFile) {
    $lines = @(Get-Content $envFile | Where-Object { $_ -notmatch "^\s*$Name\s*=" })
}
$lines += "$Name=$key"

# UTF-8 without BOM: a BOM would corrupt the first variable name for dotenv parsers.
[IO.File]::WriteAllText($envFile, (($lines -join "`n") + "`n"), (New-Object Text.UTF8Encoding $false))

Write-Host ''
Write-Host "Saved a $($key.Length)-character key as $Name in $envFile" -ForegroundColor Green
Write-Host 'You can go back to Claude now.'
