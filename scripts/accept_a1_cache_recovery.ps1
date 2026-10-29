#requires -Version 7.0
# Only fresh synthetic files below a unique, repository-external run directory are changed.
[CmdletBinding()]
param([switch]$PrepareOnly)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$repo = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
if (-not $env:PIXSTRIDE_TEST_ROOT) { throw 'Set PIXSTRIDE_TEST_ROOT to an existing directory outside Git.' }
$base = (Get-Item -LiteralPath $env:PIXSTRIDE_TEST_ROOT).FullName
if ($base -eq $repo -or $base.StartsWith($repo + [IO.Path]::DirectorySeparatorChar, [StringComparison]::OrdinalIgnoreCase)) {
    throw 'PIXSTRIDE_TEST_ROOT must be outside this repository.'
}
for ($parent = Get-Item -LiteralPath $base; $null -ne $parent; $parent = $parent.Parent) {
    if ($parent.Attributes -band [IO.FileAttributes]::ReparsePoint) { throw 'Use a non-reparse test directory.' }
}
$run = Join-Path $base ('a1-manual-' + [guid]::NewGuid().ToString('N'))
$null = New-Item -ItemType Directory -Path $run
$cacheRoot = Join-Path $run 'tiles'
$null = New-Item -ItemType Directory -Path $cacheRoot
$saved = @{}
$overrides = @{
    CARGO_TARGET_DIR = (Join-Path $run 'build')
    PIXSTRIDE_TEST_ROOT = $run
    PIXSTRIDE_TILE_CACHE = $cacheRoot
    PIXSTRIDE_TILE_CACHE_GIB = '2'
    PIXSTRIDE_STRESS_IMAGE = $null
    PIXSTRIDE_APPROVED_LARGE_CACHE = $null
}
$status = [ordered]@{ ManualGUIAcceptance = 'Pending'; AutomatedCapacity = 'Pending'; PassedSteps = @() }
function Save-Status {
    $status | ConvertTo-Json -Depth 4 | Set-Content -LiteralPath (Join-Path $run 'acceptance.json') -Encoding utf8
}
function Invoke-Cargo([string]$Label, [string[]]$CommandArguments) {
    & cargo @CommandArguments *> (Join-Path $run ($Label + '.log'))
    if ($LASTEXITCODE -ne 0) { throw "Cargo failed: $Label. Read its log in the new run directory." }
}
function Assert-OwnedEntry([string]$Path) {
    $entry = Get-Item -LiteralPath $Path
    if (-not $entry.PSIsContainer -or $entry.Parent.FullName -ne $cacheRoot -or
        $entry.Name -notmatch '^cache-[0-9a-f]{8}-[0-9a-f]{16}$' -or
        ($entry.Attributes -band [IO.FileAttributes]::ReparsePoint)) { throw 'Unsafe cache entry; preserved.' }
    $members = @(Get-ChildItem -LiteralPath $Path -Force)
    foreach ($member in $members) {
        if ($member.Name -notin @('owner', 'index.bin', 'pixels.bin') -or $member.PSIsContainer -or
            ($member.Attributes -band [IO.FileAttributes]::ReparsePoint)) { throw 'Unknown cache member; preserved.' }
    }
    $owner = [IO.File]::ReadAllBytes((Join-Path $Path 'owner'))
    if ($owner.Length -ne 8 -or [Text.Encoding]::ASCII.GetString($owner) -cne 'PXSTILE1') {
        throw 'Cannot prove cache ownership; preserved.'
    }
}
function Show-Image([string]$Step, [string]$Image, [string]$ExpectedHit = '') {
    Write-Host "`n$Step"
    Write-Host 'Inspect the image with F / 1, wheel and drag. Close this viewer normally to continue.'
    $log = Join-Path $run ($Step + '.stdout.log')
    $process = Start-Process -FilePath $binary -ArgumentList ('"' + $Image + '"') -PassThru `
        -RedirectStandardOutput $log -RedirectStandardError (Join-Path $run ($Step + '.stderr.log'))
    $process.WaitForExit()
    if ($process.ExitCode -ne 0) { throw "Viewer failed at $Step; inspect its logs." }
    if ($ExpectedHit -and (Get-Content -LiteralPath $log -Raw) -notmatch ('Tile cache: hit=' + $ExpectedHit)) {
        throw "Expected cache hit=$ExpectedHit at $Step; inspect its logs."
    }
    if ((Read-Host 'Type PASS only if display, interaction and normal close were correct') -cne 'PASS') {
        throw 'Manual acceptance not confirmed; status remains Pending.'
    }
    $status.PassedSteps += $Step
    Save-Status
}

Push-Location $repo
try {
    foreach ($key in $overrides.Keys) {
        $saved[$key] = [Environment]::GetEnvironmentVariable($key, 'Process')
        [Environment]::SetEnvironmentVariable($key, $overrides[$key], 'Process')
    }
    Save-Status
    Write-Host "New isolated acceptance directory: $run"
    Invoke-Cargo 'build' @('build', '--release', '--locked')
    Invoke-Cargo 'samples' @('test', '--release', '--locked', 'app::fullscreen_tests::release_prepare_fullscreen_samples', '--', '--exact', '--ignored', '--nocapture')
    foreach ($case in @('a1_bad_header_payload_is_charged_and_evicted_under_test_cap', 'a1_partial_delete_failure_recounts_remaining_bytes_before_admission')) {
        Invoke-Cargo $case @('test', '--release', '--locked', ('large::cache::recovery_tests::' + $case), '--', '--exact', '--nocapture')
        $result = Get-Content -LiteralPath (Join-Path $run ($case + '.log')) -Raw
        if ($result -notmatch '1 passed; 0 failed') { throw 'Capacity regression did not actually run.' }
    }
    $status.AutomatedCapacity = 'Passed'
    Save-Status
    $binary = Join-Path $env:CARGO_TARGET_DIR 'release/pixstride.exe'
    $samples = Join-Path $run 'fullscreen-gui'
    $large = Join-Path $samples 'large.png'
    $other = Join-Path $samples 'large-copy.png'
    Copy-Item -LiteralPath $large -Destination $other
    foreach ($file in @($binary, $large, $other, (Join-Path $samples 'a.jpg'), (Join-Path $samples 'b.png'),
        (Join-Path $samples 'c.bmp'), (Join-Path $samples 'infinite.gif'))) {
        if (-not (Test-Path -LiteralPath $file -PathType Leaf)) { throw 'An expected synthetic fixture is missing.' }
    }
    if ($PrepareOnly) {
        Write-Host 'Preparation and automated capacity checks passed. Manual GUI Acceptance: Pending.'
        return
    }
    Show-Image '01-cold' $large 'false'
    $entries = @(Get-ChildItem -LiteralPath $cacheRoot -Directory -Force)
    if ($entries.Count -ne 1) { throw 'Expected exactly one freshly built target; preserved all entries.' }
    $target = $entries[0].FullName
    Assert-OwnedEntry $target
    Show-Image '02-warm' $large 'true'

    # Destructive injection is restricted to the entry created and checked by this run, after process exit.
    Assert-OwnedEntry $target
    [IO.File]::Delete((Join-Path $target 'pixels.bin'))
    Show-Image '03-other-after-missing-pixels' $other 'false'
    if (Test-Path -LiteralPath $target) { throw 'The owned incomplete entry was not reclaimed.' }
    Show-Image '04-reopen-original' $large 'false'
    Assert-OwnedEntry $target
    [IO.File]::WriteAllBytes((Join-Path $target 'index.bin'), [Text.Encoding]::ASCII.GetBytes('BADINDEX'))
    Show-Image '05-bad-index-rebuild' $large 'false'
    Show-Image '06-recovered-warm' $large 'true'
    Write-Host 'Next: also use Left/Right or Ctrl+O to check JPEG, PNG, BMP and animated GIF; Space pauses GIF.'
    foreach ($name in @('a.jpg', 'b.png', 'c.bmp', 'infinite.gif')) {
        Show-Image ('07-' + $name) (Join-Path $samples $name)
    }
    [long]$payload = 0
    foreach ($entry in @(Get-ChildItem -LiteralPath $cacheRoot -Directory -Force)) {
        Assert-OwnedEntry $entry.FullName
        foreach ($name in @('index.bin', 'pixels.bin')) {
            $payload += (Get-Item -LiteralPath (Join-Path $entry.FullName $name)).Length
        }
    }
    if ($payload -gt 2GB) { throw 'Managed payload exceeds this test cache budget.' }
    $status.ManualGUIAcceptance = 'Passed'
    $status.ManagedPayloadBytes = $payload
    Save-Status
    Write-Host "Manual GUI Acceptance: Passed (owner-confirmed). Managed payload bytes: $payload"
    Write-Host 'Samples, cache and logs are retained. This script never deletes the run directory.'
} finally {
    foreach ($key in $saved.Keys) { [Environment]::SetEnvironmentVariable($key, $saved[$key], 'Process') }
    Pop-Location
}
