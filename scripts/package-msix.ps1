param(
  [Parameter(Mandatory = $true)]
  [string]$Target,

  [Parameter(Mandatory = $true)]
  [string]$Version
)

$ErrorActionPreference = "Stop"

$repoRoot = (Resolve-Path (Join-Path $PSScriptRoot ".."))
$releaseDir = Join-Path $repoRoot "src-tauri\target\$Target\release"
$exePath = Join-Path $releaseDir "givenergy-local.exe"

if (-not (Test-Path $exePath)) {
  throw "Built executable not found: $exePath"
}

function Get-MakeAppxPath {
  $cmd = Get-Command "makeappx.exe" -ErrorAction SilentlyContinue
  if ($cmd) {
    return $cmd.Source
  }

  $kitsRoot = Join-Path ${env:ProgramFiles(x86)} "Windows Kits\10\bin"
  if (-not (Test-Path $kitsRoot)) {
    throw "Windows Kits path not found: $kitsRoot"
  }

  $candidates = Get-ChildItem $kitsRoot -Directory |
    Sort-Object Name -Descending |
    ForEach-Object { Join-Path $_.FullName "x64\makeappx.exe" } |
    Where-Object { Test-Path $_ }

  if (-not $candidates) {
    throw "makeappx.exe not found in PATH or Windows Kits"
  }

  return $candidates[0]
}

function Convert-ToMsixVersion([string]$InputVersion) {
  $core = ($InputVersion -replace '^v', '') -replace '[-+].*$', ''
  $parts = $core.Split('.')
  if ($parts.Count -lt 3) {
    throw "Version must have at least major.minor.patch components: $InputVersion"
  }

  $numeric = @()
  foreach ($part in $parts[0..([Math]::Min($parts.Count - 1, 3))]) {
    if ($part -notmatch '^\d+$') {
      throw "MSIX versions must be numeric: $InputVersion"
    }
    $numeric += [int]$part
  }

  while ($numeric.Count -lt 4) {
    $numeric += 0
  }

  return ($numeric[0..3] -join '.')
}

function Get-EnvOrDefault([string]$Name, [string]$Default) {
  $value = [Environment]::GetEnvironmentVariable($Name)
  if ([string]::IsNullOrWhiteSpace($value)) {
    return $Default
  }
  return $value
}

function Escape-Xml([string]$Value) {
  return [System.Security.SecurityElement]::Escape($Value)
}

$arch = switch -Wildcard ($Target) {
  "x86_64-*" { "x64"; break }
  "aarch64-*" { "arm64"; break }
  "i686-*" { "x86"; break }
  default { throw "Unsupported MSIX target architecture: $Target" }
}

$msixVersion = Convert-ToMsixVersion $Version
$identityName = Escape-Xml (Get-EnvOrDefault "MSIX_IDENTITY_NAME" "com.givenergy.local")
$publisher = Escape-Xml (Get-EnvOrDefault "MSIX_PUBLISHER" "CN=Home Energy Manager")
$publisherDisplayName = Escape-Xml (Get-EnvOrDefault "MSIX_PUBLISHER_DISPLAY_NAME" "Stuart Harding")
$productName = Escape-Xml "Home Energy Manager"
$description = Escape-Xml "Local monitoring and control for GivEnergy solar/battery inverters"

$bundleDir = Join-Path $releaseDir "bundle\msix"
New-Item -ItemType Directory -Force -Path $bundleDir | Out-Null

$stageDir = Join-Path ([System.IO.Path]::GetTempPath()) ("givenergy-local-msix-" + [System.Guid]::NewGuid().ToString("N"))
New-Item -ItemType Directory -Force -Path $stageDir | Out-Null

try {
  Copy-Item $exePath (Join-Path $stageDir "givenergy-local.exe")

  Get-ChildItem -Path (Join-Path $releaseDir "*.dll") -File -ErrorAction SilentlyContinue |
    ForEach-Object { Copy-Item $_.FullName (Join-Path $stageDir $_.Name) }

  $distPath = Join-Path $repoRoot "dist"
  if (Test-Path $distPath) {
    Copy-Item $distPath (Join-Path $stageDir "dist") -Recurse
  }

  $resourcePath = Join-Path $releaseDir "resources"
  if (Test-Path $resourcePath) {
    Copy-Item $resourcePath (Join-Path $stageDir "resources") -Recurse
  }

  $assetsDir = Join-Path $stageDir "Assets"
  New-Item -ItemType Directory -Force -Path $assetsDir | Out-Null

  $iconsDir = Join-Path $repoRoot "src-tauri\icons"
  $assetNames = @(
    "StoreLogo.png",
    "Square44x44Logo.png",
    "Square71x71Logo.png",
    "Square150x150Logo.png",
    "Square310x310Logo.png"
  )

  foreach ($assetName in $assetNames) {
    $source = Join-Path $iconsDir $assetName
    if (-not (Test-Path $source)) {
      throw "Required MSIX asset missing: $source"
    }
    Copy-Item $source (Join-Path $assetsDir $assetName)
  }

  $manifestPath = Join-Path $stageDir "AppxManifest.xml"
  $manifest = @"
<?xml version="1.0" encoding="utf-8"?>
<Package
  xmlns="http://schemas.microsoft.com/appx/manifest/foundation/windows10"
  xmlns:uap="http://schemas.microsoft.com/appx/manifest/uap/windows10"
  xmlns:rescap="http://schemas.microsoft.com/appx/manifest/foundation/windows10/restrictedcapabilities"
  IgnorableNamespaces="uap rescap">
  <Identity Name="$identityName" Publisher="$publisher" Version="$msixVersion" ProcessorArchitecture="$arch" />
  <Properties>
    <DisplayName>$productName</DisplayName>
    <PublisherDisplayName>$publisherDisplayName</PublisherDisplayName>
    <Logo>Assets\StoreLogo.png</Logo>
  </Properties>
  <Dependencies>
    <TargetDeviceFamily Name="Windows.Desktop" MinVersion="10.0.17763.0" MaxVersionTested="10.0.22621.0" />
  </Dependencies>
  <Resources>
    <Resource Language="en-US" />
  </Resources>
  <Applications>
    <Application Id="App" Executable="givenergy-local.exe" EntryPoint="Windows.FullTrustApplication">
      <uap:VisualElements
        DisplayName="$productName"
        Description="$description"
        BackgroundColor="transparent"
        Square150x150Logo="Assets\Square150x150Logo.png"
        Square44x44Logo="Assets\Square44x44Logo.png">
        <uap:DefaultTile
          Square71x71Logo="Assets\Square71x71Logo.png"
          Square310x310Logo="Assets\Square310x310Logo.png" />
      </uap:VisualElements>
    </Application>
  </Applications>
  <Capabilities>
    <Capability Name="internetClient" />
    <Capability Name="internetClientServer" />
    <Capability Name="privateNetworkClientServer" />
    <rescap:Capability Name="runFullTrust" />
  </Capabilities>
</Package>
"@

  Set-Content -Path $manifestPath -Value $manifest -Encoding UTF8

  $safeVersion = $msixVersion
  $msixPath = Join-Path $bundleDir "HomeEnergyManager_${safeVersion}_${arch}.msix"
  $msixUploadPath = Join-Path $bundleDir "HomeEnergyManager_${safeVersion}_${arch}.msixupload"

  $makeAppx = Get-MakeAppxPath
  & $makeAppx pack /d $stageDir /p $msixPath /o
  if ($LASTEXITCODE -ne 0) {
    throw "makeappx.exe failed with exit code $LASTEXITCODE"
  }

  $uploadStage = Join-Path ([System.IO.Path]::GetTempPath()) ("givenergy-local-msixupload-" + [System.Guid]::NewGuid().ToString("N"))
  New-Item -ItemType Directory -Force -Path $uploadStage | Out-Null
  try {
    Copy-Item $msixPath (Join-Path $uploadStage (Split-Path $msixPath -Leaf))
    if (Test-Path $msixUploadPath) {
      Remove-Item $msixUploadPath -Force
    }
    $temporaryZipPath = "$msixUploadPath.zip"
    if (Test-Path $temporaryZipPath) {
      Remove-Item $temporaryZipPath -Force
    }
    Compress-Archive -Path (Join-Path $uploadStage "*") -DestinationPath $temporaryZipPath -Force
    Move-Item $temporaryZipPath $msixUploadPath
  } finally {
    Remove-Item $uploadStage -Recurse -Force -ErrorAction SilentlyContinue
  }

  Write-Host "Created MSIX: $msixPath"
  Write-Host "Created Store upload package: $msixUploadPath"
} finally {
  Remove-Item $stageDir -Recurse -Force -ErrorAction SilentlyContinue
}
