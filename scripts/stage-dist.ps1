$ErrorActionPreference = "Stop"

$repoRoot = Split-Path -Parent $PSScriptRoot
$env:PATH = "C:\msys64\mingw64\bin;C:\msys64\usr\bin;$env:USERPROFILE\.cargo\bin;$env:PATH"
$env:LIBCLANG_PATH = "C:\msys64\mingw64\bin"

Push-Location $repoRoot
try {
    Write-Host "Building gnsh-app (release)..."
    & cargo build --release -p gnsh-app
    if ($LASTEXITCODE -ne 0) {
        throw "cargo build --release -p gnsh-app failed with exit code $LASTEXITCODE"
    }

    $exeSrc = Join-Path $repoRoot "target\release\gnsh-play.exe"
    if (-not (Test-Path $exeSrc)) {
        throw "Expected release binary not found at $exeSrc"
    }

    $distDir = Join-Path $repoRoot "dist"
    if (Test-Path $distDir) {
        Remove-Item -Recurse -Force $distDir
    }
    $null = New-Item -ItemType Directory -Path $distDir

    $distExe = Join-Path $distDir "gnsh-play.exe"
    Copy-Item -Path $exeSrc -Destination $distExe -Force

    $lddExe = "C:\msys64\usr\bin\ldd.exe"
    if (-not (Test-Path $lddExe)) {
        throw "ldd.exe not found at $lddExe"
    }

    $lddOutput = & $lddExe $distExe
    if ($LASTEXITCODE -ne 0) {
        throw "ldd.exe failed with exit code $LASTEXITCODE"
    }

    $dllNames = [System.Collections.Generic.HashSet[string]]::new([System.StringComparer]::OrdinalIgnoreCase)
    foreach ($line in $lddOutput) {
        if ($line -match '(?i)/mingw64/bin/(.+?\.dll)(?:\s|$)') {
            $null = $dllNames.Add($Matches[1])
        }
    }

    $mingwBin = "C:\msys64\mingw64\bin"
    foreach ($dll in ($dllNames | Sort-Object)) {
        $srcDll = Join-Path $mingwBin $dll
        if (-not (Test-Path $srcDll)) {
            throw "Referenced MinGW64 DLL not found at $srcDll"
        }
        Copy-Item -Path $srcDll -Destination (Join-Path $distDir $dll) -Force
    }

    $stagedFiles = Get-ChildItem -Path $distDir -File
    $totalFiles = $stagedFiles.Count
    $totalBytes = ($stagedFiles | Measure-Object -Property Length -Sum).Sum
    $totalMiB = [math]::Round($totalBytes / 1MB, 2)

    Write-Host "Staged $($dllNames.Count) MinGW64 DLL(s), $totalFiles total file(s), $totalMiB MiB in $distDir"
}
finally {
    Pop-Location
}
