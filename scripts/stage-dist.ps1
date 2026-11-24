param(
    [switch]$Zip,
    [string]$OutDir = "dist-packages"
)

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

    if ($Zip) {
        $cargoToml = Get-Content -Path (Join-Path $repoRoot "Cargo.toml") -Raw
        if ($cargoToml -notmatch '(?m)^version\s*=\s*"([^"]+)"') {
            throw "Could not parse workspace version from Cargo.toml"
        }
        $version = $Matches[1]

        $outRoot = if ([System.IO.Path]::IsPathRooted($OutDir)) {
            $OutDir
        } else {
            Join-Path $repoRoot $OutDir
        }
        $null = New-Item -ItemType Directory -Path $outRoot -Force

        $zipName = "gnsh-play-v$version-win64.zip"
        $zipPath = Join-Path $outRoot $zipName
        if (Test-Path $zipPath) {
            Remove-Item -Force $zipPath
        }
        Compress-Archive -Path "$distDir\*" -DestinationPath $zipPath -Force

        $hash = (Get-FileHash -Path $zipPath -Algorithm SHA256).Hash.ToLowerInvariant()
        $shaPath = "$zipPath.sha256"
        [System.IO.File]::WriteAllText($shaPath, "$hash  $zipName`n", [System.Text.UTF8Encoding]::new($false))

        Write-Host "Packaged $zipPath (sha256: $hash)"
    }
}
finally {
    Pop-Location
}
