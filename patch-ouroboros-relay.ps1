#requires -Version 5.1

[CmdletBinding()]
param(
    [string]$ReleasePath = "",
    [string]$RepoPath = (Join-Path $HOME "Ouroboros\repo"),
    [string]$DataPath = (Join-Path $HOME "Ouroboros\data"),
    [ValidateSet(
        "gpt-5.6-sol",
        "gpt-5.6-terra",
        "gpt-5.6-luna",
        "gpt-5.5",
        "gpt-5.4",
        "gpt-5.4-mini"
    )]
    [string]$Model = "gpt-5.6-terra",
    [switch]$SkipSettings
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$script:ExpectedVersion = "6.64.0"
$script:ExpectedSourceSha = "ffcd09770438f2ebf78b3ec775ec23084e66994b"
$script:ExpectedBundleSha256 = "4bf57a342da9ca07e2255c0b93f9e2002c6cc2ab82fe48b60b623831b089866d"
$script:ExpectedPatchSha256 = "7207e4a6cdd23b698df70fe98074461d227cfd6c026f747552a5c417cfecc027"
$script:MarkerRelativePath = ".ouroboros-patches/praxis-relay-v1.json"
$script:GitPath = ""
$script:ManagedRepoPath = ""

function Write-Step {
    param([string]$Message)
    Write-Host ("[ouroboros-relay-patch] " + $Message) -ForegroundColor Cyan
}

function Get-AbsolutePath {
    param(
        [Parameter(Mandatory = $true)][string]$Path,
        [string]$BasePath = (Get-Location).Path
    )
    if ([IO.Path]::IsPathRooted($Path)) {
        return [IO.Path]::GetFullPath($Path)
    }
    return [IO.Path]::GetFullPath((Join-Path $BasePath $Path))
}

function Test-ReleaseRoot {
    param([string]$Candidate)
    if (-not $Candidate) { return $false }
    return (
        (Test-Path -LiteralPath (Join-Path $Candidate "Ouroboros.exe") -PathType Leaf) -and
        (Test-Path -LiteralPath (Join-Path $Candidate "_internal\repo_bundle_manifest.json") -PathType Leaf) -and
        (Test-Path -LiteralPath (Join-Path $Candidate "_internal\repo.bundle") -PathType Leaf)
    )
}

function Resolve-ReleaseRoot {
    param([string]$RequestedPath)
    if ($RequestedPath) {
        $resolved = Get-AbsolutePath -Path $RequestedPath
        if (-not (Test-ReleaseRoot $resolved)) {
            throw "ReleasePath is not an extracted Ouroboros Windows release: $resolved"
        }
        return $resolved
    }

    $scriptParent = Split-Path -Parent $PSScriptRoot
    $scriptGrandParent = Split-Path -Parent $scriptParent
    $candidates = @(
        (Get-Location).Path,
        (Join-Path (Get-Location).Path "Ouroboros"),
        $PSScriptRoot,
        (Join-Path $scriptParent "Ouroboros"),
        (Join-Path $scriptGrandParent "Ouroboros")
    ) | Select-Object -Unique

    foreach ($candidate in $candidates) {
        if (Test-ReleaseRoot $candidate) {
            return [IO.Path]::GetFullPath($candidate)
        }
    }
    throw "Ouroboros release was not found. Pass -ReleasePath with the folder containing Ouroboros.exe."
}

function Find-GitHubDesktopGit {
    $desktopRoot = Join-Path $env:LOCALAPPDATA "GitHubDesktop"
    if (-not (Test-Path -LiteralPath $desktopRoot -PathType Container)) {
        throw "GitHub Desktop is required. Install it first; the GUI will not be opened."
    }

    $apps = @(Get-ChildItem -LiteralPath $desktopRoot -Directory -Filter "app-*" -ErrorAction SilentlyContinue | ForEach-Object {
        $versionText = $_.Name.Substring(4)
        try {
            [pscustomobject]@{ Directory = $_.FullName; Version = [version]$versionText }
        } catch {
            $null
        }
    } | Sort-Object Version -Descending)

    foreach ($app in $apps) {
        $candidate = Join-Path $app.Directory "resources\app\git\cmd\git.exe"
        if (Test-Path -LiteralPath $candidate -PathType Leaf) {
            return $candidate
        }
    }
    throw "GitHub Desktop's bundled git.exe was not found. The GitHub Desktop GUI is not used by this script."
}

function Invoke-Git {
    param(
        [Parameter(Mandatory = $true)][string[]]$Arguments,
        [switch]$AllowFailure,
        [switch]$Echo
    )
    $raw = @()
    $exitCode = -1
    $nativeErrorPreference = $ErrorActionPreference
    try {
        $ErrorActionPreference = "Continue"
        $raw = @(& $script:GitPath -C $script:ManagedRepoPath @Arguments 2>&1)
        $exitCode = $LASTEXITCODE
    } finally {
        $ErrorActionPreference = $nativeErrorPreference
    }
    $lines = @($raw | ForEach-Object { [string]$_ })
    if ($Echo -and $lines.Count -gt 0) {
        $lines | ForEach-Object { Write-Host $_ }
    }
    $result = [pscustomobject]@{
        ExitCode = $exitCode
        Lines = $lines
        Text = ($lines -join [Environment]::NewLine)
    }
    if ($exitCode -ne 0 -and -not $AllowFailure) {
        throw "GitHub Desktop Git failed (exit $exitCode): git $($Arguments -join ' ')$([Environment]::NewLine)$($result.Text)"
    }
    return $result
}

function Get-EnvironmentSnapshot {
    param([string[]]$Names)
    $snapshot = @{}
    foreach ($name in $Names) {
        $snapshot[$name] = [Environment]::GetEnvironmentVariable($name, "Process")
    }
    return $snapshot
}

function Restore-EnvironmentSnapshot {
    param([hashtable]$Snapshot)
    foreach ($name in $Snapshot.Keys) {
        [Environment]::SetEnvironmentVariable($name, $Snapshot[$name], "Process")
    }
}

function Invoke-FreshBootstrap {
    param(
        [string]$BundleRoot,
        [string]$EmbeddedPython,
        [string]$ResolvedRepoPath,
        [string]$ResolvedDataPath
    )
    $defaultRepo = Get-AbsolutePath -Path (Join-Path $HOME "Ouroboros\repo")
    if ($ResolvedRepoPath -ne $defaultRepo) {
        throw "A custom -RepoPath must already be an initialized Git worktree. Missing: $ResolvedRepoPath"
    }

    Write-Step "Bootstrapping the managed repository from the signed release bundle"
    New-Item -ItemType Directory -Path $ResolvedDataPath -Force | Out-Null
    $names = @("OUROBOROS_PACKAGED_BUNDLE_ROOT", "PYTHONPATH", "PYTHONDONTWRITEBYTECODE", "PYTHONPYCACHEPREFIX")
    $snapshot = Get-EnvironmentSnapshot $names
    try {
        $env:OUROBOROS_PACKAGED_BUNDLE_ROOT = $BundleRoot
        $env:PYTHONPATH = $BundleRoot
        $env:PYTHONDONTWRITEBYTECODE = "1"
        $env:PYTHONPYCACHEPREFIX = Join-Path $ResolvedDataPath "state\patch-pycache"
        $bootstrapCode = "from ouroboros.packaged_cli import resolve_packaged_runtime, _bootstrap_runtime; _bootstrap_runtime(resolve_packaged_runtime())"
        $nativeErrorPreference = $ErrorActionPreference
        try {
            $ErrorActionPreference = "Continue"
            & $EmbeddedPython -c $bootstrapCode
            $bootstrapExit = $LASTEXITCODE
        } finally {
            $ErrorActionPreference = $nativeErrorPreference
        }
        if ($bootstrapExit -ne 0) {
            throw "The packaged Ouroboros bootstrap failed with exit code $bootstrapExit."
        }
    } finally {
        Restore-EnvironmentSnapshot $snapshot
    }
}

function Assert-RepoClean {
    $status = (Invoke-Git -Arguments @("status", "--porcelain=v1", "--untracked-files=all")).Text.Trim()
    if ($status) {
        throw "The Ouroboros worktree is not clean. Commit or move these changes first:$([Environment]::NewLine)$status"
    }
}

function Remove-BootstrapPin {
    $gitDirText = (Invoke-Git -Arguments @("rev-parse", "--absolute-git-dir")).Text.Trim()
    if (-not $gitDirText) { throw "Could not resolve the Ouroboros Git directory." }
    $pinPath = Join-Path $gitDirText "ouroboros-bootstrap-pending"
    if (Test-Path -LiteralPath $pinPath -PathType Leaf) {
        Remove-Item -LiteralPath $pinPath -Force
        Write-Step "Cleared the one-shot bootstrap pin after the patch commit"
    }
}

function Clear-OuroborosBytecodeCache {
    param(
        [string]$ResolvedRepoPath,
        [string]$ResolvedDataPath
    )
    $repoRoot = [IO.Path]::GetFullPath($ResolvedRepoPath).TrimEnd("\") + "\"
    $dataRoot = [IO.Path]::GetFullPath($ResolvedDataPath).TrimEnd("\") + "\"
    $removed = 0

    $repoCaches = @(Get-ChildItem -LiteralPath $ResolvedRepoPath -Directory -Filter "__pycache__" -Recurse -Force -ErrorAction SilentlyContinue |
        Sort-Object { $_.FullName.Length } -Descending)
    foreach ($cache in $repoCaches) {
        if (($cache.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) {
            Write-Warning "Skipping reparse-point bytecode cache: $($cache.FullName)"
            continue
        }
        $resolvedCache = [IO.Path]::GetFullPath($cache.FullName)
        if (-not $resolvedCache.StartsWith($repoRoot, [StringComparison]::OrdinalIgnoreCase) -or $cache.Name -ne "__pycache__") {
            throw "Refusing to remove an unverified bytecode path: $resolvedCache"
        }
        Remove-Item -LiteralPath $resolvedCache -Recurse -Force
        $removed += 1
    }

    $runtimeCache = Join-Path $ResolvedDataPath "state\pycache"
    if (Test-Path -LiteralPath $runtimeCache -PathType Container) {
        $runtimeItem = Get-Item -LiteralPath $runtimeCache -Force
        if (($runtimeItem.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) {
            Write-Warning "Skipping reparse-point runtime bytecode cache: $runtimeCache"
        } else {
            $resolvedRuntimeCache = [IO.Path]::GetFullPath($runtimeCache)
            if (-not $resolvedRuntimeCache.StartsWith($dataRoot, [StringComparison]::OrdinalIgnoreCase)) {
                throw "Refusing to remove an unverified runtime bytecode path: $resolvedRuntimeCache"
            }
            Remove-Item -LiteralPath $resolvedRuntimeCache -Recurse -Force
            $removed += 1
        }
    }
    Write-Step "Invalidated $removed stale Python bytecode cache director$(if ($removed -eq 1) { 'y' } else { 'ies' })"
}

function Invoke-PatchTests {
    param(
        [string]$EmbeddedPython,
        [string]$BundleRoot,
        [string]$ResolvedDataPath
    )
    $tempBase = [IO.Path]::GetFullPath($env:TEMP)
    $testRoot = Join-Path $tempBase ("ouroboros-relay-patch-" + [Guid]::NewGuid().ToString("N"))
    $testData = Join-Path $testRoot "data"
    $compileLog = Join-Path $testRoot "compile.log"
    $testLog = Join-Path $testRoot "pytest.log"
    New-Item -ItemType Directory -Path $testData -Force | Out-Null

    $names = @(
        "PYTHONPATH", "PYTHONDONTWRITEBYTECODE", "PYTHONPYCACHEPREFIX",
        "OUROBOROS_REPO_DIR", "OUROBOROS_DATA_DIR", "OUROBOROS_SETTINGS_PATH"
    )
    $snapshot = Get-EnvironmentSnapshot $names
    try {
        $env:PYTHONPATH = $script:ManagedRepoPath + ";" + $BundleRoot
        $env:PYTHONDONTWRITEBYTECODE = "1"
        $env:PYTHONPYCACHEPREFIX = Join-Path $testRoot "pycache"
        $env:OUROBOROS_REPO_DIR = $script:ManagedRepoPath
        $env:OUROBOROS_DATA_DIR = $testData
        $env:OUROBOROS_SETTINGS_PATH = Join-Path $testData "settings.json"
        $tests = @(
            "tests/test_onboarding_wizard.py",
            "tests/test_model_catalog_api.py",
            "tests/test_settings_ui_syntax.py",
            "tests/test_llm_provider_routing.py",
            "tests/test_capability_evidence.py"
        )
        Write-Step "Running the release-embedded Python test gate"
        Push-Location $script:ManagedRepoPath
        try {
            $compileCode = @'
import json
import os
from pathlib import Path
from supervisor import git_ops
from supervisor.update_merge import update_restart_smoke
git_ops.init(
    Path(os.environ['OUROBOROS_REPO_DIR']),
    Path(os.environ['OUROBOROS_DATA_DIR']),
    '',
)
result = update_restart_smoke()
print(json.dumps(result, sort_keys=True))
raise SystemExit(0 if result.get('ok') else 1)
'@
            Write-Step "Running Ouroboros's native py_compile and restart-import gate"
            $nativeErrorPreference = $ErrorActionPreference
            try {
                $ErrorActionPreference = "Continue"
                & $EmbeddedPython -c $compileCode *> $compileLog
                $compileExit = $LASTEXITCODE
            } finally {
                $ErrorActionPreference = $nativeErrorPreference
            }
            Get-Content -LiteralPath $compileLog -Tail 50 | ForEach-Object { Write-Host $_ }
            if ($compileExit -ne 0) {
                throw "Ouroboros restart compile/import gate failed with exit code $compileExit."
            }

            $nativeErrorPreference = $ErrorActionPreference
            try {
                $ErrorActionPreference = "Continue"
                & $EmbeddedPython -m pytest -q @tests *> $testLog
                $testExit = $LASTEXITCODE
            } finally {
                $ErrorActionPreference = $nativeErrorPreference
            }
        } finally {
            Pop-Location
        }
        $tailCount = if ($testExit -eq 0) { 25 } else { 100 }
        Get-Content -LiteralPath $testLog -Tail $tailCount | ForEach-Object { Write-Host $_ }
        if ($testExit -ne 0) {
            throw "Patch tests failed with exit code $testExit."
        }
    } finally {
        Restore-EnvironmentSnapshot $snapshot
        $resolvedTestRoot = [IO.Path]::GetFullPath($testRoot)
        $safePrefix = $tempBase.TrimEnd("\") + "\"
        if ($resolvedTestRoot.StartsWith($safePrefix, [StringComparison]::OrdinalIgnoreCase)) {
            Remove-Item -LiteralPath $resolvedTestRoot -Recurse -Force -ErrorAction SilentlyContinue
        }
    }
}

function Set-JsonProperty {
    param(
        [Parameter(Mandatory = $true)]$Object,
        [Parameter(Mandatory = $true)][string]$Name,
        $Value
    )
    if ($null -eq $Object.PSObject.Properties[$Name]) {
        $Object | Add-Member -NotePropertyName $Name -NotePropertyValue $Value
    } else {
        $Object.$Name = $Value
    }
}

function Restore-SettingsState {
    param($State)
    if ($null -eq $State) { return }
    if ($State.Existed) {
        Copy-Item -LiteralPath $State.BackupPath -Destination $State.SettingsPath -Force
    } elseif (Test-Path -LiteralPath $State.SettingsPath) {
        Remove-Item -LiteralPath $State.SettingsPath -Force
    }
}

function Update-PraxisRelaySettings {
    param(
        [string]$ResolvedDataPath,
        [string]$SelectedModel
    )
    New-Item -ItemType Directory -Path $ResolvedDataPath -Force | Out-Null
    $settingsPath = Join-Path $ResolvedDataPath "settings.json"
    $existed = Test-Path -LiteralPath $settingsPath -PathType Leaf
    $backupPath = ""
    if ($existed) {
        $stamp = Get-Date -Format "yyyyMMdd-HHmmss"
        $backupPath = Join-Path $ResolvedDataPath ("settings.json.praxis-relay." + $stamp + ".bak")
        Copy-Item -LiteralPath $settingsPath -Destination $backupPath
        $raw = Get-Content -LiteralPath $settingsPath -Raw -Encoding UTF8
        $settings = if ($raw.Trim()) { $raw | ConvertFrom-Json } else { [pscustomobject]@{} }
    } else {
        $settings = [pscustomobject]@{}
    }

    $state = [pscustomobject]@{
        Existed = $existed
        SettingsPath = $settingsPath
        BackupPath = $backupPath
    }
    try {
        $mainModel = "openai-compatible::" + $SelectedModel
        $lightModel = "openai-compatible::gpt-5.4-mini"
        $values = [ordered]@{
            OPENAI_COMPATIBLE_BASE_URL = "http://localhost:5011"
            OPENAI_COMPATIBLE_API_KEY = "auto"
            OUROBOROS_MODEL = $mainModel
            OUROBOROS_MODEL_HEAVY = $mainModel
            OUROBOROS_MODEL_LIGHT = $lightModel
            OUROBOROS_MODEL_VISION = $mainModel
            OUROBOROS_MODEL_CONSCIOUSNESS = $mainModel
            OUROBOROS_MODEL_FALLBACKS = $lightModel
            USE_LOCAL_MAIN = $false
            USE_LOCAL_HEAVY = $false
            USE_LOCAL_LIGHT = $false
            USE_LOCAL_CONSCIOUSNESS = $false
            USE_LOCAL_FALLBACK = $false
        }
        foreach ($entry in $values.GetEnumerator()) {
            Set-JsonProperty -Object $settings -Name $entry.Key -Value $entry.Value
        }

        $json = $settings | ConvertTo-Json -Depth 100
        $tempPath = $settingsPath + ".praxis-relay.tmp"
        $utf8NoBom = New-Object System.Text.UTF8Encoding($false)
        [IO.File]::WriteAllText($tempPath, $json + [Environment]::NewLine, $utf8NoBom)
        if ($existed) {
            try {
                [IO.File]::Replace($tempPath, $settingsPath, $null)
            } catch {
                Move-Item -LiteralPath $tempPath -Destination $settingsPath -Force
            }
        } else {
            Move-Item -LiteralPath $tempPath -Destination $settingsPath
        }

        $verify = Get-Content -LiteralPath $settingsPath -Raw -Encoding UTF8 | ConvertFrom-Json
        if ($verify.OPENAI_COMPATIBLE_BASE_URL -ne "http://localhost:5011" -or
            $verify.OPENAI_COMPATIBLE_API_KEY -ne "auto" -or
            $verify.OUROBOROS_MODEL -ne $mainModel) {
            throw "The persisted settings did not pass verification."
        }
        Write-Step "Configured settings.json for Praxis Relay; all unrelated settings were preserved"
        return $state
    } catch {
        Restore-SettingsState $state
        throw
    }
}

function Test-ConfiguredRoute {
    param(
        [string]$EmbeddedPython,
        [string]$BundleRoot,
        [string]$SelectedModel
    )
    $names = @("PYTHONPATH", "OPENAI_COMPATIBLE_BASE_URL", "OPENAI_COMPATIBLE_API_KEY", "PATCH_EXPECTED_MODEL")
    $snapshot = Get-EnvironmentSnapshot $names
    try {
        $env:PYTHONPATH = $script:ManagedRepoPath + ";" + $BundleRoot
        $env:OPENAI_COMPATIBLE_BASE_URL = "http://localhost:5011"
        $env:OPENAI_COMPATIBLE_API_KEY = "auto"
        $env:PATCH_EXPECTED_MODEL = $SelectedModel
        $code = @'
import os
from ouroboros.llm import LLMClient
model = os.environ['PATCH_EXPECTED_MODEL']
target = LLMClient()._resolve_remote_target('openai-compatible::' + model)
assert target['provider'] == 'openai-compatible', target
assert target['base_url'] == 'http://localhost:5011', target
assert target['api_key'] == 'auto', target
assert target['resolved_model'] == model, target
print('route-smoke: openai-compatible -> localhost:5011 -> ' + model)
'@
        $nativeErrorPreference = $ErrorActionPreference
        try {
            $ErrorActionPreference = "Continue"
            $output = @(& $EmbeddedPython -c $code 2>&1)
            $routeExit = $LASTEXITCODE
        } finally {
            $ErrorActionPreference = $nativeErrorPreference
        }
        $output | ForEach-Object { Write-Host ([string]$_) }
        if ($routeExit -ne 0) {
            throw "The configured provider route smoke test failed."
        }
    } finally {
        Restore-EnvironmentSnapshot $snapshot
    }
}

function Test-RelayIfRunning {
    $expected = @("gpt-5.6-sol", "gpt-5.6-terra", "gpt-5.6-luna", "gpt-5.5", "gpt-5.4", "gpt-5.4-mini")
    try {
        $catalog = Invoke-RestMethod -Uri "http://localhost:5011/v1/models" -Headers @{ Authorization = "Bearer auto" } -TimeoutSec 2
        $actual = @($catalog.data | ForEach-Object { [string]$_.id })
        $missing = @($expected | Where-Object { $actual -notcontains $_ })
        if ($missing.Count -gt 0) {
            Write-Warning ("Relay is running, but its model catalog is missing: " + ($missing -join ", "))
        } else {
            Write-Step "Live relay catalog matches all six selector models"
        }
    } catch {
        Write-Warning "Praxis Relay is not reachable on localhost:5011 yet. The patch is installed; start the relay before Ouroboros."
    }
}

Write-Step "Resolving the released Ouroboros runtime"
$releaseRoot = Resolve-ReleaseRoot -RequestedPath $ReleasePath
$bundleRoot = Join-Path $releaseRoot "_internal"
$manifestPath = Join-Path $bundleRoot "repo_bundle_manifest.json"
$bundlePath = Join-Path $bundleRoot "repo.bundle"
$versionPath = Join-Path $bundleRoot "VERSION"
$embeddedPython = Join-Path $bundleRoot "python-standalone\python.exe"
$patchPath = Join-Path $PSScriptRoot "patches\ouroboros-v6.64.0-praxis-relay.patch"

foreach ($required in @($manifestPath, $bundlePath, $versionPath, $embeddedPython, $patchPath)) {
    if (-not (Test-Path -LiteralPath $required -PathType Leaf)) {
        throw "Required release or patch file is missing: $required"
    }
}

$version = (Get-Content -LiteralPath $versionPath -Raw -Encoding UTF8).Trim()
$manifest = Get-Content -LiteralPath $manifestPath -Raw -Encoding UTF8 | ConvertFrom-Json
if ($version -ne $script:ExpectedVersion -or $manifest.app_version -ne $script:ExpectedVersion) {
    throw "This patch targets Ouroboros $($script:ExpectedVersion), but the selected release is $version."
}
if ($manifest.source_sha -ne $script:ExpectedSourceSha) {
    throw "Unexpected release source SHA: $($manifest.source_sha)"
}
$bundleHash = (Get-FileHash -LiteralPath $bundlePath -Algorithm SHA256).Hash.ToLowerInvariant()
if ($bundleHash -ne $script:ExpectedBundleSha256 -or $bundleHash -ne ([string]$manifest.bundle_sha256).ToLowerInvariant()) {
    throw "The embedded repo.bundle failed its SHA-256 integrity check."
}
$patchHash = (Get-FileHash -LiteralPath $patchPath -Algorithm SHA256).Hash.ToLowerInvariant()
if ($patchHash -ne $script:ExpectedPatchSha256) {
    throw "The patch artifact failed its SHA-256 integrity check."
}

$running = @(Get-Process -Name "Ouroboros" -ErrorAction SilentlyContinue)
if ($running.Count -gt 0) {
    throw "Close Ouroboros before patching. Running process IDs: $($running.Id -join ', ')"
}

$script:GitPath = Find-GitHubDesktopGit
$gitDirectory = Split-Path -Parent $script:GitPath
$env:PATH = $gitDirectory + ";" + $env:PATH
$script:ManagedRepoPath = Get-AbsolutePath -Path $RepoPath
$resolvedDataPath = Get-AbsolutePath -Path $DataPath
Write-Step "Using GitHub Desktop Git: $($script:GitPath)"

if (-not (Test-Path -LiteralPath (Join-Path $script:ManagedRepoPath ".git"))) {
    Invoke-FreshBootstrap -BundleRoot $bundleRoot -EmbeddedPython $embeddedPython -ResolvedRepoPath $script:ManagedRepoPath -ResolvedDataPath $resolvedDataPath
}
if (-not (Test-Path -LiteralPath (Join-Path $script:ManagedRepoPath ".git"))) {
    throw "Managed Ouroboros Git worktree was not found: $($script:ManagedRepoPath)"
}

Assert-RepoClean
$currentBranch = (Invoke-Git -Arguments @("symbolic-ref", "--quiet", "--short", "HEAD") -AllowFailure).Text.Trim()
if (-not $currentBranch) {
    throw "The managed Ouroboros repository is in detached HEAD state. Check out its runtime branch first."
}

$markerPath = Join-Path $script:ManagedRepoPath ($script:MarkerRelativePath.Replace("/", "\"))
$alreadyPatched = Test-Path -LiteralPath $markerPath -PathType Leaf
$settingsState = $null
$backupBranch = ""
$patchCommit = ""

if ($alreadyPatched) {
    $marker = Get-Content -LiteralPath $markerPath -Raw -Encoding UTF8 | ConvertFrom-Json
    if ($marker.id -ne "praxis-relay-openai-compatible" -or $marker.target_version -ne $script:ExpectedVersion) {
        throw "An unknown Praxis Relay patch marker already exists: $markerPath"
    }
    $patchCommit = (Invoke-Git -Arguments @("log", "-1", "--format=%H", "--", $script:MarkerRelativePath)).Text.Trim()
    if (-not $patchCommit) { throw "The patch marker is not backed by a Git commit." }
    Write-Step "Patch is already committed at $patchCommit; no empty commit will be created"
    if (-not $SkipSettings) {
        $settingsState = Update-PraxisRelaySettings -ResolvedDataPath $resolvedDataPath -SelectedModel $Model
        Test-ConfiguredRoute -EmbeddedPython $embeddedPython -BundleRoot $bundleRoot -SelectedModel $Model
    }
    Assert-RepoClean
    Clear-OuroborosBytecodeCache -ResolvedRepoPath $script:ManagedRepoPath -ResolvedDataPath $resolvedDataPath
    Remove-BootstrapPin
} else {
    $originalHead = (Invoke-Git -Arguments @("rev-parse", "HEAD")).Text.Trim()
    $sourceRef = "refs/ouroboros-patch/release-v6.64.0"
    $bundleRefspec = "HEAD:$sourceRef"
    $sourceCommitRef = "$sourceRef^{commit}"
    Invoke-Git -Arguments @("fetch", "--force", "--no-tags", $bundlePath, $bundleRefspec) -Echo | Out-Null
    $fetchedSource = (Invoke-Git -Arguments @("rev-parse", $sourceCommitRef)).Text.Trim()
    if ($fetchedSource -ne $script:ExpectedSourceSha) {
        throw "The release bundle resolved to $fetchedSource instead of the expected source commit."
    }

    $headIsAncestor = (Invoke-Git -Arguments @("merge-base", "--is-ancestor", $originalHead, $script:ExpectedSourceSha) -AllowFailure).ExitCode -eq 0
    $sourceIsAncestor = (Invoke-Git -Arguments @("merge-base", "--is-ancestor", $script:ExpectedSourceSha, $originalHead) -AllowFailure).ExitCode -eq 0
    if (-not $headIsAncestor -and -not $sourceIsAncestor) {
        throw "The current branch diverges from the v6.64.0 release lineage."
    }

    $shortHead = $originalHead.Substring(0, 8)
    $backupBranch = "ouroboros-patch-backup/" + (Get-Date -Format "yyyyMMdd-HHmmss") + "-" + $shortHead
    Invoke-Git -Arguments @("branch", $backupBranch, $originalHead) | Out-Null
    Write-Step "Created rollback branch $backupBranch"

    try {
        if ($headIsAncestor -and $originalHead -ne $script:ExpectedSourceSha) {
            Write-Step "Fast-forwarding the clean managed branch to the v6.64.0 release source"
            Invoke-Git -Arguments @("merge", "--ff-only", $sourceRef) | Out-Null
        }

        $prePatchHead = (Invoke-Git -Arguments @("rev-parse", "HEAD")).Text.Trim()
        Write-Step "Applying the signed mail patch; git am will create the required commit"
        Invoke-Git -Arguments @("am", "--3way", $patchPath) -Echo | Out-Null
        $patchCommit = (Invoke-Git -Arguments @("rev-parse", "HEAD")).Text.Trim()
        if (-not $patchCommit -or $patchCommit -eq $prePatchHead) {
            throw "git am did not create a new patch commit."
        }

        $changedFiles = @((Invoke-Git -Arguments @("diff-tree", "--no-commit-id", "--name-only", "-r", $patchCommit)).Lines)
        if ($changedFiles -notcontains $script:MarkerRelativePath) {
            throw "The new commit does not contain the expected patch marker."
        }

        Invoke-PatchTests -EmbeddedPython $embeddedPython -BundleRoot $bundleRoot -ResolvedDataPath $resolvedDataPath
        Assert-RepoClean
        if (-not $SkipSettings) {
            $settingsState = Update-PraxisRelaySettings -ResolvedDataPath $resolvedDataPath -SelectedModel $Model
            Test-ConfiguredRoute -EmbeddedPython $embeddedPython -BundleRoot $bundleRoot -SelectedModel $Model
        }
        Assert-RepoClean
        Clear-OuroborosBytecodeCache -ResolvedRepoPath $script:ManagedRepoPath -ResolvedDataPath $resolvedDataPath
        Remove-BootstrapPin
    } catch {
        Write-Warning "Patch transaction failed; restoring the exact clean starting HEAD."
        Invoke-Git -Arguments @("am", "--abort") -AllowFailure | Out-Null
        Invoke-Git -Arguments @("reset", "--hard", $originalHead) -Echo | Out-Null
        Restore-SettingsState $settingsState
        throw
    }
}

$finalHead = (Invoke-Git -Arguments @("rev-parse", "HEAD")).Text.Trim()
Assert-RepoClean
Test-RelayIfRunning

Write-Host ""
Write-Host "Praxis Relay patch is ready." -ForegroundColor Green
Write-Host "  Ouroboros repo : $($script:ManagedRepoPath)"
Write-Host "  Runtime branch : $currentBranch"
Write-Host "  Patch commit   : $patchCommit"
Write-Host "  Current HEAD   : $finalHead"
if ($backupBranch) { Write-Host "  Rollback branch: $backupBranch" }
if ($settingsState -and $settingsState.BackupPath) { Write-Host "  Settings backup: $($settingsState.BackupPath)" }
Write-Host "  Base URL       : http://localhost:5011"
Write-Host "  API key        : auto"
Write-Host "  Main model     : openai-compatible::$Model"
Write-Host ""
Write-Host "Start Praxis Relay first, then launch Ouroboros.exe." -ForegroundColor Yellow
