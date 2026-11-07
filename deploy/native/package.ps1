# Run with PowerShell 7 after building the native release binaries.
param(
    [string]$OutputDirectory = 'dist',
    [string]$BinaryDirectory = 'target/release',
    [string]$Architecture = [Runtime.InteropServices.RuntimeInformation]::OSArchitecture.ToString().ToUpperInvariant()
)

$ErrorActionPreference = 'Stop'
$source = (Resolve-Path -LiteralPath "$PSScriptRoot/../..").Path
$suffix = if ($IsWindows) { '.exe' } else { '' }
$platform = if ($IsWindows) { 'Windows' } elseif ($IsMacOS) { 'macOS' } else { 'Linux' }
if ($IsWindows -and $Architecture -cne 'X64') {
    throw 'Windows release packages currently support x64.'
}

$collector = Join-Path $BinaryDirectory "searchcord$suffix"
$identification = & (Resolve-Path -LiteralPath $collector).Path --version
if ($LASTEXITCODE -ne 0 -or $identification -cnotmatch '\Asearchcord ([0-9]+\.[0-9]+\.[0-9]+)\z') {
    throw 'Could not read the version from the release build.'
}
$version = $Matches[1]
$output = (New-Item -ItemType Directory -Path $OutputDirectory -Force).FullName
$extension = if ($IsWindows) { 'zip' } else { 'tar.gz' }
$archive = Join-Path $output "searchcord-$platform-$Architecture.$extension"
if (Test-Path -LiteralPath $archive) {
    throw "Package already exists at $archive."
}

$staging = Join-Path $output ('.package-' + [Guid]::NewGuid().ToString('N'))
try {
    $program = if ($IsWindows) { Join-Path $staging "app/$version" } else { $staging }
    [void][IO.Directory]::CreateDirectory($program)
    foreach ($binary in @('searchcord', 'searchcord-search', 'searchcord-snapshot', 'searchcord-package-search')) {
        Copy-Item -LiteralPath (Join-Path $BinaryDirectory "$binary$suffix") -Destination $program
    }
    Copy-Item -LiteralPath "$source/LICENSE", "$source/deploy/search/THIRD_PARTY_NOTICES.md" -Destination $program
    Copy-Item -LiteralPath "$PSScriptRoot/README.txt" -Destination $staging
    if ($IsWindows) {
        Copy-Item -LiteralPath "$PSScriptRoot/update.ps1" -Destination $program
        Copy-Item -LiteralPath "$PSScriptRoot/Update Searchcord.bat" -Destination $staging
        Copy-Item -LiteralPath "$source/start.bat" -Destination "$staging/Start Searchcord.bat"
        Copy-Item -LiteralPath "$source/start-search.bat" -Destination "$staging/Start Searchcord Search.bat"
        [IO.File]::WriteAllText((Join-Path $staging '.searchcord-version'), "$version`r`n")
        [IO.Compression.ZipFile]::CreateFromDirectory($staging, $archive)
    } else {
        tar -czf $archive -C $staging .
        if ($LASTEXITCODE -ne 0) { throw 'Could not create the release archive.' }
    }
    Write-Output $archive
} finally {
    if ([IO.Directory]::Exists($staging)) {
        [IO.Directory]::Delete($staging, $true)
    }
}
