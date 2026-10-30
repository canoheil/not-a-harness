# nah installer for Windows (PowerShell 5+).
#
#   irm https://raw.githubusercontent.com/canoheil/not-a-harness/main/install.ps1 | iex
#
# Downloads the latest release, installs nah.exe into %LOCALAPPDATA%\nah\bin
# and adds it to your user PATH, so `nah` can be run from any terminal.
# Re-running the script updates nah.
$ErrorActionPreference = 'Stop'

$Repo = 'canoheil/not-a-harness'
if ($env:NAH_DOWNLOAD_BASE) {
    $Base = $env:NAH_DOWNLOAD_BASE
} else {
    $Base = "https://github.com/$Repo/releases/latest/download"
}

switch ($(if ($env:PROCESSOR_ARCHITEW6432) { $env:PROCESSOR_ARCHITEW6432 } else { $env:PROCESSOR_ARCHITECTURE })) {
    'AMD64' { $target = 'x86_64-pc-windows-msvc' }
    'ARM64' { throw 'no prebuilt binary for Windows ARM64 yet - build from source: cargo build --release' }
    default { throw "unsupported CPU architecture: $($env:PROCESSOR_ARCHITECTURE)" }
}
$asset = "nah-$target.zip"
$url = "$Base/$asset"

# older Windows PowerShell defaults to TLS 1.0
[Net.ServicePointManager]::SecurityProtocol = `
    [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12

Write-Host "downloading $url ..."
$tmpDir = Join-Path ([IO.Path]::GetTempPath()) ("nah-install-" + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $tmpDir | Out-Null
try {
    Invoke-WebRequest -Uri $url -OutFile (Join-Path $tmpDir $asset) -UseBasicParsing
    Expand-Archive -Path (Join-Path $tmpDir $asset) -DestinationPath $tmpDir -Force

    $exe = Get-ChildItem -Path $tmpDir -Recurse -Filter 'nah.exe' | Select-Object -First 1
    if (-not $exe) { throw 'the archive did not contain nah.exe' }

    if ($env:NAH_INSTALL_DIR) {
        $binDir = $env:NAH_INSTALL_DIR
    } else {
        $binDir = Join-Path $env:LOCALAPPDATA 'nah\bin'
    }
    New-Item -ItemType Directory -Force -Path $binDir | Out-Null
    Copy-Item $exe.FullName (Join-Path $binDir 'nah.exe') -Force
    Write-Host "installed: $binDir\nah.exe"

    # make `nah` reachable from any terminal (persistent user PATH)
    $userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
    if (-not $userPath) { $userPath = '' }
    $parts = @($userPath -split ';' | Where-Object { $_ })
    if ($parts -notcontains $binDir) {
        if ($userPath) {
            $newPath = "$userPath;$binDir"
        } else {
            $newPath = "$binDir"
        }
        [Environment]::SetEnvironmentVariable('Path', $newPath, 'User')

        # tell Explorer (and terminals launched from it) about the change
        Add-Type -Namespace Nah -Name Env -MemberDefinition @'
[DllImport("user32.dll", SetLastError = true, CharSet = CharSet.Auto)]
public static extern IntPtr SendMessageTimeout(IntPtr hWnd, uint Msg, UIntPtr wParam, string lParam, uint fuFlags, uint uTimeout, out UIntPtr lpdwResult);
'@
        $result = [UIntPtr]::Zero
        [void][Nah.Env]::SendMessageTimeout([IntPtr]0xffff, 0x1A, [UIntPtr]::Zero, 'Environment', 2, 5000, [ref]$result)
        Write-Host "added $binDir to your user PATH"
        Write-Host "restart this terminal, then just type: nah"
    } else {
        Write-Host "PATH already contains $binDir"
    }

    # current session too, so nah works here after the restart hint
    $env:Path = "$env:Path;$binDir"
    Write-Host ''
    Write-Host 'done! type: nah' -ForegroundColor Green
}
finally {
    Remove-Item -Recurse -Force $tmpDir -ErrorAction SilentlyContinue
}
