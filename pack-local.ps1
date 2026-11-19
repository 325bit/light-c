# Local packaging script without signing. Derived from pack.ps1:
# same build + artifact collection + SHA256 output, but no private key,
# no signer invocation, and updater artifacts disabled for this build.
#
# Updater artifacts are disabled via a temporary --config override so the
# committed tauri.conf.json stays untouched.

$ErrorActionPreference = "Stop"

Write-Host "========================================" -ForegroundColor Cyan
Write-Host "  LightC Local Build Script (no signing)" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan
Write-Host ""

$ProjectRoot = $PSScriptRoot
$TauriConfigPath = Join-Path $ProjectRoot "src-tauri\tauri.conf.json"

# "Run with PowerShell" closes the console instantly on any error, which made
# failures undiagnosable ("window flashes and disappears"). Keep a full
# transcript on disk and always wait for Enter before closing.
$LogPath = Join-Path $ProjectRoot "pack-local-run.log"
try { Start-Transcript -Path $LogPath -Force | Out-Null } catch { }

try {

if (-not (Test-Path $TauriConfigPath)) {
    Write-Host "Error: Cannot find tauri.conf.json at $TauriConfigPath" -ForegroundColor Red
    exit 1
}

Write-Host "[1/5] Reading version..." -ForegroundColor Yellow

$RawJson = [System.IO.File]::ReadAllText($TauriConfigPath, [System.Text.Encoding]::UTF8)
if ($RawJson.Length -gt 0 -and [int][char]$RawJson[0] -eq 65279) {
    $RawJson = $RawJson.Substring(1)
}

$TauriConfig = $RawJson | ConvertFrom-Json
$Version = $TauriConfig.version
$ProductName = $TauriConfig.productName

if ([string]::IsNullOrEmpty($Version)) {
    Write-Host "Error: Cannot read version from tauri.conf.json" -ForegroundColor Red
    exit 1
}

Write-Host "  Product: $ProductName" -ForegroundColor White
Write-Host "  Version: v$Version" -ForegroundColor White
Write-Host ""

# Never sign: clear any signing variables inherited from the shell so the
# build cannot accidentally produce updater artifacts.
Remove-Item Env:\TAURI_SIGNING_PRIVATE_KEY -ErrorAction SilentlyContinue
Remove-Item Env:\TAURI_SIGNING_PRIVATE_KEY_PASSWORD -ErrorAction SilentlyContinue
Remove-Item Env:\TAURI_PRIVATE_KEY -ErrorAction SilentlyContinue
Remove-Item Env:\TAURI_PRIVATE_KEY_PASSWORD -ErrorAction SilentlyContinue

Write-Host "[2/5] Building..." -ForegroundColor Yellow
Write-Host "  Running: npm run tauri build (updater artifacts disabled)" -ForegroundColor Gray

# createUpdaterArtifacts is true in tauri.conf.json; building without a private
# key would fail. Override it for this build only via a temp config file.
$OverrideConfigPath = Join-Path $env:TEMP ("tauri-local-no-sign-" + [guid]::NewGuid().ToString("N") + ".json")
@{ bundle = @{ createUpdaterArtifacts = $false } } | ConvertTo-Json |
    Out-File -FilePath $OverrideConfigPath -Encoding utf8

try {
    Push-Location $ProjectRoot
    # Note: npm writes "npm notice ..." to stderr, and this script starts with
    # $ErrorActionPreference = "Stop". Without 2>&1, PowerShell treats that
    # output as a NativeCommandError and aborts even though the build succeeded.
    & cmd /c "npm run tauri build -- --config ""$OverrideConfigPath"" 2>&1"
    $buildExit = $LASTEXITCODE
} finally {
    Pop-Location
    Remove-Item $OverrideConfigPath -Force -ErrorAction SilentlyContinue
}

if ($buildExit -ne 0) {
    Write-Host "Error: Build failed, exit code: $buildExit" -ForegroundColor Red
    exit 1
}

Write-Host "  Build completed!" -ForegroundColor Green
Write-Host ""

Write-Host "[3/5] Packaging artifacts..." -ForegroundColor Yellow

$ReleaseDir = Join-Path $ProjectRoot "src-tauri\target\release"
$BundleMsiDir = Join-Path $ReleaseDir "bundle\msi"
$BundleNsisDir = Join-Path $ReleaseDir "bundle\nsis"
$DistReleaseDir = Join-Path $ProjectRoot "dist_release"
$PortableDir = Join-Path $DistReleaseDir ("LightC_" + $Version + "_Portable")
# Local deployment target (live portable install with user data inside).
$DeployDir = "D:\tools\LightC_portable"

if (Test-Path $DistReleaseDir) {
    Write-Host "  Cleaning old dist_release..." -ForegroundColor Gray
    Remove-Item $DistReleaseDir -Recurse -Force
}
New-Item -ItemType Directory -Path $DistReleaseDir -Force | Out-Null
Write-Host "  Created: $DistReleaseDir" -ForegroundColor Gray

Write-Host "  Processing MSI installer..." -ForegroundColor Gray
$MsiFiles = Get-ChildItem -Path $BundleMsiDir -Filter "*.msi" -ErrorAction SilentlyContinue
if ($null -eq $MsiFiles -or $MsiFiles.Count -eq 0) {
    Write-Host "  Warning: No MSI found, skipping..." -ForegroundColor Yellow
} else {
    $TargetMsiName = "LightC_" + $Version + "_x64_Installer.msi"
    Copy-Item $MsiFiles[0].FullName (Join-Path $DistReleaseDir $TargetMsiName) -Force
    Write-Host "    Copied: $TargetMsiName" -ForegroundColor White
}

Write-Host "  Processing NSIS installer..." -ForegroundColor Gray
$NsisFiles = Get-ChildItem -Path $BundleNsisDir -Filter "*.exe" -ErrorAction SilentlyContinue
if ($null -eq $NsisFiles -or $NsisFiles.Count -eq 0) {
    Write-Host "  Warning: No NSIS installer found, skipping..." -ForegroundColor Yellow
} else {
    $TargetNsisName = "LightC_" + $Version + "_x64_Setup.exe"
    Copy-Item $NsisFiles[0].FullName (Join-Path $DistReleaseDir $TargetNsisName) -Force
    Write-Host "    Copied: $TargetNsisName" -ForegroundColor White
}

Write-Host "  Processing Portable version..." -ForegroundColor Gray
New-Item -ItemType Directory -Path $PortableDir -Force | Out-Null

$ExePath = Join-Path $ReleaseDir "LightC.exe"
if (-not (Test-Path $ExePath)) {
    Write-Host "Error: Cannot find LightC.exe at $ExePath" -ForegroundColor Red
    exit 1
}
Copy-Item $ExePath $PortableDir -Force
Write-Host "    Copied: LightC.exe" -ForegroundColor White

# Keep the legacy marker; the manifest lets newer builds verify that the data directory uses relative paths.
Set-Content -Path (Join-Path $PortableDir "LightC.portable") -Value "portable" -Encoding UTF8
$PortableManifest = [ordered]@{
    schema_version = 1
    mode = "portable"
    data_layout = "relative"
}
$PortableManifest | ConvertTo-Json | Out-File -FilePath (Join-Path $PortableDir "LightC.portable.json") -Encoding utf8
Write-Host "    Created: portable mode manifest" -ForegroundColor White

$ResourcesDir = Join-Path $ReleaseDir "resources"
if (Test-Path $ResourcesDir) {
    $ResItems = Get-ChildItem $ResourcesDir -ErrorAction SilentlyContinue
    if ($null -ne $ResItems -and $ResItems.Count -gt 0) {
        Copy-Item $ResourcesDir (Join-Path $PortableDir "resources") -Recurse -Force
        Write-Host "    Copied: resources" -ForegroundColor White
    }
}

$DllFiles = Get-ChildItem -Path $ReleaseDir -Filter "*.dll" -ErrorAction SilentlyContinue
if ($null -ne $DllFiles) {
    foreach ($dll in $DllFiles) {
        Copy-Item $dll.FullName $PortableDir -Force
        Write-Host "    Copied: $($dll.Name)" -ForegroundColor White
    }
}

$ZipFileName = "LightC_" + $Version + "_x64_Portable.zip"
$ZipFilePath = Join-Path $DistReleaseDir $ZipFileName
Write-Host "  Compressing portable version..." -ForegroundColor Gray
Compress-Archive -Path $PortableDir -DestinationPath $ZipFilePath -Force
Write-Host "    Created: $ZipFileName" -ForegroundColor White

# Local adaptation: deploy to D:\tools\LightC_portable after packaging.
Write-Host "[4/5] Deploying to $DeployDir..." -ForegroundColor Yellow

if (-not (Test-Path $DeployDir)) {
    New-Item -ItemType Directory -Path $DeployDir -Force | Out-Null
    Write-Host "  Created deploy directory: $DeployDir" -ForegroundColor Gray
}

# Stop a running instance, but only if it is the deployed copy; an instance
# launched from elsewhere is left alone and deployment aborts.
$DeployExe = Join-Path $DeployDir "LightC.exe"
$Running = Get-Process LightC -ErrorAction SilentlyContinue
if ($null -ne $Running) {
    $DeployedRunning = $Running | Where-Object { $_.Path -eq $DeployExe }
    if ($null -ne $DeployedRunning) {
        Write-Host "  Stopping running LightC from $DeployDir..." -ForegroundColor Gray
        $DeployedRunning | Stop-Process -Force
        Wait-Process -Name LightC -ErrorAction SilentlyContinue -Timeout 10
    } else {
        Write-Host "Error: LightC.exe is running from another location:" -ForegroundColor Red
        $Running | ForEach-Object { Write-Host "  $($_.Path)" -ForegroundColor Red }
        Write-Host "Close it before deploying." -ForegroundColor Red
        exit 1
    }
}

# Keep the deploy dir's user data (config/, data/, webview/) untouched:
# only replace program files. Existing convention: back up the current exe.
if (Test-Path $DeployExe) {
    Copy-Item $DeployExe (Join-Path $DeployDir "LightC.exe.bak") -Force
    Write-Host "  Backed up old exe to LightC.exe.bak" -ForegroundColor Gray
}

Copy-Item (Join-Path $PortableDir "LightC.exe") $DeployExe -Force
Write-Host "  Deployed: LightC.exe" -ForegroundColor White
Copy-Item (Join-Path $PortableDir "LightC.portable") $DeployDir -Force
Copy-Item (Join-Path $PortableDir "LightC.portable.json") $DeployDir -Force
Write-Host "  Deployed: portable mode markers" -ForegroundColor White

$DeployResourcesDir = Join-Path $PortableDir "resources"
if (Test-Path $DeployResourcesDir) {
    Copy-Item $DeployResourcesDir (Join-Path $DeployDir "resources") -Recurse -Force
    Write-Host "  Deployed: resources" -ForegroundColor White
}

$DeployDllFiles = Get-ChildItem -Path $PortableDir -Filter "*.dll" -ErrorAction SilentlyContinue
foreach ($dll in $DeployDllFiles) {
    Copy-Item $dll.FullName $DeployDir -Force
    Write-Host "  Deployed: $($dll.Name)" -ForegroundColor White
}

Write-Host "  Deploy complete. User data (config/, data/, webview/) was not touched." -ForegroundColor Green
Remove-Item $PortableDir -Recurse -Force

Write-Host ""

Write-Host "[5/5] Calculating SHA256 checksums..." -ForegroundColor Yellow

$Sha256SumsPath = Join-Path $DistReleaseDir "SHA256SUMS.txt"
$Sha256Lines = [System.Collections.Generic.List[string]]::new()

$FilesToHash = Get-ChildItem -Path $DistReleaseDir -File | Where-Object { $_.Name -ne "SHA256SUMS.txt" }

foreach ($file in $FilesToHash) {
    $hash = Get-FileHash -Path $file.FullName -Algorithm SHA256
    $hashLine = "$($hash.Hash)  $($file.Name)"
    $Sha256Lines.Add($hashLine)
    Write-Host "  $($file.Name)" -ForegroundColor White
    Write-Host "    SHA256: $($hash.Hash)" -ForegroundColor Gray
}

[System.IO.File]::WriteAllLines($Sha256SumsPath, $Sha256Lines, (New-Object System.Text.UTF8Encoding $false))
Write-Host "  Generated: SHA256SUMS.txt" -ForegroundColor White
Write-Host ""

Write-Host "========================================" -ForegroundColor Green
Write-Host "  Build Success! (unsigned, local only)" -ForegroundColor Green
Write-Host "  Artifacts are in dist_release folder" -ForegroundColor Green
Write-Host "  NOTE: No signatures generated; do not" -ForegroundColor Yellow
Write-Host "        upload these to GitHub Release." -ForegroundColor Yellow
Write-Host "========================================" -ForegroundColor Green
Write-Host ""

Write-Host "Artifacts:" -ForegroundColor Cyan
Get-ChildItem -Path $DistReleaseDir | ForEach-Object {
    if ($_.Length -gt 1MB) {
        $size = "{0:N2} MB" -f ($_.Length / 1MB)
    } else {
        $size = "{0:N2} KB" -f ($_.Length / 1KB)
    }
    Write-Host "  - $($_.Name) ($size)" -ForegroundColor White
}

Write-Host ""
Write-Host "Version: v$Version" -ForegroundColor Cyan
Write-Host "Path:    $DistReleaseDir" -ForegroundColor Cyan

} catch {
    Write-Host ""
    Write-Host "========================================" -ForegroundColor Red
    Write-Host "  Script failed:" -ForegroundColor Red
    Write-Host "  $($_.Exception.Message)" -ForegroundColor Red
    Write-Host "  At: $($_.InvocationInfo.PositionMessage)" -ForegroundColor DarkGray
    Write-Host "  Full log: $LogPath" -ForegroundColor Yellow
    Write-Host "========================================" -ForegroundColor Red
} finally {
    try { Stop-Transcript | Out-Null } catch { }
    Write-Host ""
    Read-Host "Finished. Press Enter to close this window"
}
