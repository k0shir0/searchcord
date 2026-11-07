$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12

$repository = 'k0shir0/searchcord'
$appRoot = Split-Path -Parent $PSScriptRoot
$installRoot = Split-Path -Parent $appRoot
$versionFile = Join-Path $installRoot '.searchcord-version'
$payload = @('searchcord.exe', 'searchcord-search.exe', 'searchcord-snapshot.exe',
    'searchcord-package-search.exe', 'update.ps1', 'LICENSE', 'THIRD_PARTY_NOTICES.md')

try {
    $lock = [IO.File]::Open((Join-Path $appRoot '.update-lock'), 'OpenOrCreate', 'ReadWrite', 'None')
} catch [IO.IOException] {
    throw 'Could not lock the update folder. Close any other Searchcord updater and try again.'
}

$staging = Join-Path $appRoot ('.update-' + [Guid]::NewGuid().ToString('N'))
try {
    $installed = [IO.File]::ReadAllText($versionFile).Trim()
    if ($installed -cnotmatch '\A[0-9]+\.[0-9]+\.[0-9]+\z') {
        throw 'The installed version could not be read. Extract the full Windows download again.'
    }

    Write-Host "Checking for Searchcord updates (installed $installed)..."
    try {
        $release = Invoke-RestMethod -UseBasicParsing -TimeoutSec 30 `
            -Uri "https://api.github.com/repos/$repository/releases/latest" `
            -Headers @{ Accept = 'application/vnd.github+json' }
    } catch {
        if ($null -ne $_.Exception.Response -and [int]$_.Exception.Response.StatusCode -eq 404) {
            Write-Host 'There is no published Searchcord update yet.'
            return
        }
        throw
    }
    if ($release.draft -or $release.prerelease -or $release.tag_name -cnotmatch '\Av([0-9]+\.[0-9]+\.[0-9]+)\z') {
        throw 'GitHub returned an unrecognized Searchcord release.'
    }
    $version = $Matches[1]
    if ([version]$version -le [version]$installed) {
        Write-Host "Searchcord $installed is up to date."
        return
    }

    $running = Get-Process -Name searchcord, searchcord-search -ErrorAction SilentlyContinue |
        Where-Object { $_.Path -and $_.Path.StartsWith("$installRoot\", [StringComparison]::OrdinalIgnoreCase) }
    if ($running) {
        throw 'Close Searchcord and Searchcord Search, then open Update Searchcord again.'
    }

    $assetName = 'searchcord-Windows-X64.zip'
    $url = "https://github.com/$repository/releases/download/v$version/$assetName"
    $asset = @($release.assets | Where-Object { $_.name -ceq $assetName -and $_.state -eq 'uploaded' })
    if ($asset.Count -ne 1 -or $asset[0].browser_download_url -cne $url -or
        $asset[0].digest -cnotmatch '\Asha256:([0-9a-fA-F]{64})\z') {
        throw "The Windows download for Searchcord $version is unavailable or missing its checksum."
    }
    $checksum = $Matches[1]
    $download = Join-Path $staging $assetName
    $unpacked = Join-Path $staging 'program'
    [void][IO.Directory]::CreateDirectory($unpacked)
    Write-Host "Downloading Searchcord $version..."
    Invoke-WebRequest -UseBasicParsing -TimeoutSec 300 -Uri $url -OutFile $download
    if ((Get-FileHash -LiteralPath $download -Algorithm SHA256).Hash -ne $checksum) {
        throw 'The download did not match the release checksum. Try updating again.'
    }

    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $zip = [IO.Compression.ZipFile]::OpenRead($download)
    try {
        foreach ($file in $payload) {
            $entry = $zip.GetEntry("app/$version/$file")
            if ($null -eq $entry -or $entry.Length -eq 0) {
                throw "The Windows download is missing $file."
            }
            [IO.Compression.ZipFileExtensions]::ExtractToFile($entry, (Join-Path $unpacked $file))
        }
    } finally {
        $zip.Dispose()
    }

    # A version becomes active only after its full program directory is in place.
    $destination = Join-Path $appRoot $version
    if ([IO.Directory]::Exists($destination)) {
        # An interrupted update can leave a complete directory before activation.
        foreach ($file in $payload) {
            if ((Get-FileHash -LiteralPath (Join-Path $destination $file)).Hash -ne
                (Get-FileHash -LiteralPath (Join-Path $unpacked $file)).Hash) {
                throw "The existing $version folder differs from the release. Update could not activate it."
            }
        }
    } else {
        [IO.Directory]::Move($unpacked, $destination)
    }
    $nextVersion = Join-Path $staging 'version'
    [IO.File]::WriteAllText($nextVersion, "$version`r`n")
    [IO.File]::Replace($nextVersion, $versionFile, (Join-Path $appRoot '.previous-version'))
    Write-Host "Updated to Searchcord $version. Open Start Searchcord to continue."
} finally {
    $lock.Dispose()
    if ([IO.Directory]::Exists($staging)) {
        try {
            [IO.Directory]::Delete($staging, $true)
        } catch [IO.IOException] {
            Write-Warning "Could not remove the temporary download at $staging."
        }
    }
}
