<#
.SYNOPSIS
    Collect the Phase-4 scalability metrics for #22 (redb/SQLite decision) and
    #155 (zero-copy rkyv read-back) into one paste-ready markdown file.

.DESCRIPTION
    Runs three layers of measurement and assembles them into a single report:

      1. Scenarios  — `scale-bench --bin scenario`, one process per measurement,
                      so peak RSS is attributable. Covers the memory axes and
                      the 1M point that criterion cannot afford to repeat.
      2. Criterion  — `cargo bench -p scale-bench`, the statistical source of
                      truth for latency at 665/100K.
      3. Report bin — the existing single-shot markdown snapshot.

    Everything runs `--offline`, so it works on an isolated machine as long as
    the crates are already in the local cargo registry cache.

.PARAMETER Full
    Include the 1M-node basis. Run last on purpose: it allocates multiple GB and
    may swap or OOM on a 16 GB machine — which is itself a #22 datapoint, so a
    failure there is recorded as a row, not treated as a broken run.

.PARAMETER Model
    Path to an elaborated hierarchy.json for the realism-anchor `real` basis
    (see docs/benchmarking.md for how to produce one offline).

.PARAMETER SkipCriterion
    Skip layer 2. Useful for a quick re-run of just the memory axes.

.PARAMETER SkipReport
    Skip layer 3.

.PARAMETER Out
    Output file. Defaults to core/target/scale-bench/metrics-<timestamp>.md.

.EXAMPLE
    ./scale-bench-collect.ps1
.EXAMPLE
    ./scale-bench-collect.ps1 -Full -Model C:\path\to\hierarchy.json
#>
[CmdletBinding()]
param(
    [switch]$Full,
    [string]$Model = "",
    [switch]$SkipCriterion,
    [switch]$SkipReport,
    [string]$Out = ""
)

$ErrorActionPreference = "Stop"

$coreDir = Split-Path -Parent $PSScriptRoot
$repoDir = Split-Path -Parent $coreDir
$stamp = Get-Date -Format "yyyyMMdd-HHmmss"

if ([string]::IsNullOrWhiteSpace($Out)) {
    $outDir = Join-Path $coreDir "target\scale-bench"
    if (-not (Test-Path $outDir)) { New-Item -ItemType Directory -Force $outDir | Out-Null }
    $Out = Join-Path $outDir "metrics-$stamp.md"
}

# --- helpers ---------------------------------------------------------------

# Native exe via Start-Process so stdout/stderr and the exit code are captured
# without PS 5.1's NativeCommandError wrapping (see CLAUDE.md shell notes).
function Invoke-Captured {
    param(
        [Parameter(Mandatory = $true)][string]$Exe,
        [string[]]$ExeArgs = @(),
        [string]$WorkDir = $null
    )
    $outFile = [System.IO.Path]::GetTempFileName()
    $errFile = [System.IO.Path]::GetTempFileName()
    $startArgs = @{
        FilePath               = $Exe
        NoNewWindow            = $true
        Wait                   = $true
        PassThru               = $true
        RedirectStandardOutput = $outFile
        RedirectStandardError  = $errFile
    }
    if ($ExeArgs.Count -gt 0) { $startArgs.ArgumentList = $ExeArgs }
    if ($WorkDir) { $startArgs.WorkingDirectory = $WorkDir }

    $began = Get-Date
    $proc = Start-Process @startArgs
    $elapsed = ((Get-Date) - $began).TotalSeconds
    $stdout = ""
    $stderr = ""
    # -Encoding UTF8 explicitly: PS 5.1 reads as ANSI by default, which turns the
    # report bin's `µs` column headers into mojibake.
    if (Test-Path $outFile) { $stdout = (Get-Content $outFile -Raw -Encoding UTF8) }
    if (Test-Path $errFile) { $stderr = (Get-Content $errFile -Raw -Encoding UTF8) }
    Remove-Item $outFile, $errFile -Force -ErrorAction SilentlyContinue

    [pscustomobject]@{
        ExitCode = $proc.ExitCode
        Stdout   = $stdout
        Stderr   = $stderr
        Seconds  = $elapsed
    }
}

# One scenario run -> the parsed JSON record, or a failure row. A crash (OOM at
# 1M) is data: it is recorded, never swallowed.
function Invoke-Scenario {
    param(
        [Parameter(Mandatory = $true)][string]$Exe,
        [Parameter(Mandatory = $true)][string]$Basis,
        [Parameter(Mandatory = $true)][string]$Mode,
        [string[]]$Extra = @()
    )
    $label = "$Basis/$Mode"
    if ($Extra.Count -gt 0) { $label += " " + ($Extra -join " ") }
    Write-Host "  scenario $label" -ForegroundColor DarkGray

    $exeArgs = @("--basis", $Basis, "--mode", $Mode) + $Extra
    $r = Invoke-Captured -Exe $Exe -ExeArgs $exeArgs -WorkDir $coreDir

    $line = ($r.Stdout -split "`n" | Where-Object { $_.Trim().StartsWith("{") } | Select-Object -First 1)
    if ($r.ExitCode -eq 0 -and $line) {
        $rec = $line | ConvertFrom-Json
        Add-Member -InputObject $rec -NotePropertyName ok -NotePropertyValue $true -Force
        return $rec
    }

    $why = ($r.Stderr -split "`n" | Where-Object { $_.Trim() } | Select-Object -Last 1)
    if (-not $why) { $why = "no output (exit $($r.ExitCode)) - likely killed by the OS" }
    Write-Host "    FAILED: $($why.Trim())" -ForegroundColor Yellow
    [pscustomobject]@{
        basis     = $Basis
        mode      = $Mode
        ok        = $false
        exit_code = $r.ExitCode
        error     = $why.Trim()
        wall_ms   = $r.Seconds * 1000
    }
}

function Get-Field {
    param($Record, [string]$Name, [string]$Fallback = "-")
    if ($null -eq $Record) { return $Fallback }
    $p = $Record.PSObject.Properties[$Name]
    if ($null -eq $p -or $null -eq $p.Value) { return $Fallback }
    if ($p.Value -is [double]) { return ("{0:N2}" -f $p.Value) }
    return "$($p.Value)"
}

function Find-Row {
    param($Rows, [string]$Basis, [string]$Mode)
    $Rows | Where-Object { $_.basis -eq $Basis -and $_.mode -eq $Mode -and $_.ok } | Select-Object -First 1
}

# --- environment -----------------------------------------------------------

Write-Host "Collecting environment facts..." -ForegroundColor Cyan
$cpu = Get-CimInstance Win32_Processor | Select-Object -First 1
$cs = Get-CimInstance Win32_ComputerSystem
$os = Get-CimInstance Win32_OperatingSystem
$rustc = (& rustc -V)
$cargo = (& cargo -V)
Push-Location $repoDir
$gitCommit = (& git rev-parse --short HEAD)
$gitBranch = (& git rev-parse --abbrev-ref HEAD)
$gitDirty = (& git status --porcelain)
Pop-Location
$dirtyNote = "clean"
if ($gitDirty) { $dirtyNote = "DIRTY ($(@($gitDirty).Count) changed files)" }

# --- build -----------------------------------------------------------------

Write-Host "Building scale-bench (release, offline)..." -ForegroundColor Cyan
$build = Invoke-Captured -Exe "cargo" -ExeArgs @("build", "--release", "-p", "scale-bench", "--offline") -WorkDir $coreDir
if ($build.ExitCode -ne 0) {
    Write-Host $build.Stderr
    throw "cargo build failed (exit $($build.ExitCode)). On an isolated machine this usually means a crate is missing from the local registry cache."
}
$scenarioExe = Join-Path $coreDir "target\release\scenario.exe"
if (-not (Test-Path $scenarioExe)) { $scenarioExe = Join-Path $coreDir "target\release\scenario" }
if (-not (Test-Path $scenarioExe)) { throw "scenario binary not found under core/target/release" }

# --- layer 1: scenarios ----------------------------------------------------

# 1M runs LAST so the smaller bases are already recorded if it swaps or dies.
$bases = @("golden", "665", "100K")
if ($Model) {
    if (-not (Test-Path $Model)) { throw "-Model '$Model' not found" }
    $env:SCALE_BENCH_MODEL = (Resolve-Path $Model).Path
    $bases += "real"
}
if ($Full) { $bases += "1M" }

$loadModes = @("prepare", "from_slice", "cache_hit", "access_checked", "access_unchecked")
$rows = @()

Write-Host "Running scenarios (bases: $($bases -join ', '))..." -ForegroundColor Cyan
foreach ($basis in $bases) {
    foreach ($mode in $loadModes) {
        $rows += Invoke-Scenario -Exe $scenarioExe -Basis $basis -Mode $mode
    }
    # A basis whose prepare failed cannot support nav/match; skip rather than
    # emit a wall of identical "run prepare first" errors.
    if (Find-Row -Rows $rows -Basis $basis -Mode "prepare") {
        $rows += Invoke-Scenario -Exe $scenarioExe -Basis $basis -Mode "nav"
        foreach ($sig in @(1000, 10000, 100000)) {
            $rows += Invoke-Scenario -Exe $scenarioExe -Basis $basis -Mode "match" -Extra @("--signals", "$sig")
        }
    }
}

# --- layer 2: criterion ----------------------------------------------------

$criterionRows = @()
$criterionNote = "skipped (-SkipCriterion)"
if (-not $SkipCriterion) {
    Write-Host "Running criterion (this takes several minutes)..." -ForegroundColor Cyan
    $criterionStart = Get-Date
    # Reduced sample size keeps the 100K load/matcher groups to minutes rather
    # than tens of minutes; the 1M sizes stay out of criterion entirely (the
    # scenarios above own that point).
    $benchArgs = @("bench", "-p", "scale-bench", "--offline", "--",
        "--warm-up-time", "1", "--measurement-time", "3", "--sample-size", "20")
    $bench = Invoke-Captured -Exe "cargo" -ExeArgs $benchArgs -WorkDir $coreDir
    $criterionNote = "cargo bench -p scale-bench -- --warm-up-time 1 --measurement-time 3 --sample-size 20 (exit $($bench.ExitCode))"

    $critDir = Join-Path $coreDir "target\criterion"
    if (Test-Path $critDir) {
        # Only results written by THIS run - the dir keeps prior runs around and
        # a stale estimate silently mixed in would be worse than a missing row.
        Get-ChildItem -Path $critDir -Recurse -Filter "estimates.json" |
            Where-Object { $_.DirectoryName -like "*\new" -and $_.LastWriteTime -ge $criterionStart } |
            ForEach-Object {
                $est = Get-Content $_.FullName -Raw | ConvertFrom-Json
                $metaPath = Join-Path $_.DirectoryName "benchmark.json"
                $id = $_.DirectoryName
                if (Test-Path $metaPath) {
                    $id = (Get-Content $metaPath -Raw | ConvertFrom-Json).full_id
                }
                $criterionRows += [pscustomobject]@{
                    id        = $id
                    mean_ms   = $est.mean.point_estimate / 1e6
                    median_ms = $est.median.point_estimate / 1e6
                    stddev_ms = $est.std_dev.point_estimate / 1e6
                }
            }
    }
}

# --- layer 3: report bin ---------------------------------------------------

$reportOut = "_skipped (-SkipReport)_"
if (-not $SkipReport) {
    Write-Host "Running the single-shot report bin..." -ForegroundColor Cyan
    $reportArgs = @("run", "-p", "scale-bench", "--release", "--offline", "--bin", "report")
    if ($Full) { $reportArgs += @("--", "--full") }
    $rep = Invoke-Captured -Exe "cargo" -ExeArgs $reportArgs -WorkDir $coreDir
    if ($rep.ExitCode -eq 0) {
        $reportOut = (($rep.Stdout -split "`n" | Where-Object { $_.Trim().StartsWith("|") }) -join "`n")
    }
    else {
        $reportOut = "_report bin failed (exit $($rep.ExitCode)): $($rep.Stderr.Trim())_"
    }
}

# --- assemble --------------------------------------------------------------

Write-Host "Writing $Out" -ForegroundColor Cyan
$md = New-Object System.Collections.Generic.List[string]
$md.Add("# scale-bench metrics - #22 / #155")
$md.Add("")
$md.Add("Generated $(Get-Date -Format 'yyyy-MM-dd HH:mm:ss') by ``core/scripts/scale-bench-collect.ps1``.")
$md.Add("")
$md.Add("## Environment")
$md.Add("")
$md.Add("| key | value |")
$md.Add("|---|---|")
$md.Add("| CPU | $($cpu.Name) |")
$md.Add("| cores / threads | $($cpu.NumberOfCores) / $($cpu.NumberOfLogicalProcessors) |")
$md.Add("| RAM total | $([math]::Round($cs.TotalPhysicalMemory / 1GB, 1)) GB |")
$md.Add("| RAM free at start | $([math]::Round($os.FreePhysicalMemory / 1MB, 1)) GB |")
$md.Add("| OS | $($os.Caption) $($os.Version) |")
$md.Add("| rustc | $rustc |")
$md.Add("| cargo | $cargo |")
$md.Add("| git | $gitBranch @ $gitCommit ($dirtyNote) |")
$md.Add("| bases run | $($bases -join ', ') |")
$md.Add("| real model | $(if ($Model) { $Model } else { 'none' }) |")
$md.Add("")

$md.Add("## Load + memory (one process per row; peak RSS is that process's high-water mark)")
$md.Add("")
$md.Add("| basis | mode | nodes | edges | json MB | cache MB | wall ms | peak RSS MB | end RSS MB | note |")
$md.Add("|---|---|--:|--:|--:|--:|--:|--:|--:|---|")
foreach ($r in $rows) {
    if ($r.mode -eq "nav" -or $r.mode -eq "match") { continue }
    $note = ""
    if (-not $r.ok) { $note = "**FAILED** - $($r.error)" }
    $md.Add("| $($r.basis) | $($r.mode) | $(Get-Field $r 'nodes') | $(Get-Field $r 'edges') | $(Get-Field $r 'json_mb') | $(Get-Field $r 'cache_mb') | $(Get-Field $r 'wall_ms') | $(Get-Field $r 'peak_rss_mb') | $(Get-Field $r 'end_rss_mb') | $note |")
}
$md.Add("")

$md.Add("### Derived: where warm-load time goes (#155)")
$md.Add("")
$md.Add("``cache_hit`` = mmap + validating access + **deserialize** + index build. ``access_checked`` stops after validation; ``access_unchecked`` skips validation too. A true zero-copy read-back removes the deserialize step but still needs the indices.")
$md.Add("")
$md.Add("| basis | from_slice ms | cache_hit ms | access_checked ms | access_unchecked ms | validate ms | deserialize+index ms | cache_hit peak RSS MB | access peak RSS MB |")
$md.Add("|---|--:|--:|--:|--:|--:|--:|--:|--:|")
foreach ($basis in $bases) {
    $fs = Find-Row -Rows $rows -Basis $basis -Mode "from_slice"
    $ch = Find-Row -Rows $rows -Basis $basis -Mode "cache_hit"
    $ac = Find-Row -Rows $rows -Basis $basis -Mode "access_checked"
    $au = Find-Row -Rows $rows -Basis $basis -Mode "access_unchecked"
    $validate = "-"
    if ($ac -and $au) { $validate = "{0:N2}" -f ($ac.wall_ms - $au.wall_ms) }
    $deser = "-"
    if ($ch -and $ac) { $deser = "{0:N2}" -f ($ch.wall_ms - $ac.wall_ms) }
    $md.Add("| $basis | $(Get-Field $fs 'wall_ms') | $(Get-Field $ch 'wall_ms') | $(Get-Field $ac 'wall_ms') | $(Get-Field $au 'wall_ms') | $validate | $deser | $(Get-Field $ch 'peak_rss_mb') | $(Get-Field $ac 'peak_rss_mb') |")
}
$md.Add("")

$md.Add("## Scripted navigation (working set + query spread) (#22)")
$md.Add("")
$md.Add("| basis | load ms | RSS after load MB | peak RSS MB | scope_graph p50/p95 us | expand p50/p95 us | path p95 us | source p95 us | cone ms | cone nodes | hot fanout |")
$md.Add("|---|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|")
foreach ($r in ($rows | Where-Object { $_.mode -eq "nav" })) {
    if (-not $r.ok) {
        $md.Add("| $($r.basis) | **FAILED** - $($r.error) | | | | | | | | | |")
        continue
    }
    $sg = "$(Get-Field $r 'scope_graph_p50_us') / $(Get-Field $r 'scope_graph_p95_us')"
    $ex = "$(Get-Field $r 'expand_p50_us') / $(Get-Field $r 'expand_p95_us')"
    $md.Add("| $($r.basis) | $(Get-Field $r 'load_ms') | $(Get-Field $r 'rss_after_load_mb') | $(Get-Field $r 'peak_rss_mb') | $sg | $ex | $(Get-Field $r 'nodes_at_path_p95_us') | $(Get-Field $r 'nodes_at_source_p95_us') | $(Get-Field $r 'cone_ms') | $(Get-Field $r 'cone_nodes') | $(Get-Field $r 'hot_fanout') |")
}
$md.Add("")

$md.Add("## Matcher vs trace signal count (#22)")
$md.Add("")
$md.Add("| basis | signals | wall ms | peak RSS MB | hit % | note |")
$md.Add("|---|--:|--:|--:|--:|---|")
foreach ($r in ($rows | Where-Object { $_.mode -eq "match" })) {
    $note = ""
    if (-not $r.ok) { $note = "**FAILED** - $($r.error)" }
    $md.Add("| $($r.basis) | $(Get-Field $r 'signals') | $(Get-Field $r 'wall_ms') | $(Get-Field $r 'peak_rss_mb') | $(Get-Field $r 'hit_rate_pct') | $note |")
}
$md.Add("")

$md.Add("## Criterion (statistical, 665 + 100K)")
$md.Add("")
# Braces required: `$criterionNote_` would parse as a (nonexistent) variable name.
$md.Add("_${criterionNote}_")
$md.Add("")
if ($criterionRows.Count -gt 0) {
    $md.Add("| bench | mean ms | median ms | std dev ms |")
    $md.Add("|---|--:|--:|--:|")
    foreach ($c in ($criterionRows | Sort-Object id)) {
        $md.Add("| $($c.id) | $('{0:N3}' -f $c.mean_ms) | $('{0:N3}' -f $c.median_ms) | $('{0:N3}' -f $c.stddev_ms) |")
    }
}
else {
    $md.Add("_no criterion estimates from this run_")
}
$md.Add("")

$md.Add("## Single-shot report bin")
$md.Add("")
$md.Add($reportOut)
$md.Add("")

$md.Add("## Raw scenario records")
$md.Add("")
$md.Add('```json')
foreach ($r in $rows) { $md.Add(($r | ConvertTo-Json -Compress -Depth 4)) }
$md.Add('```')

Set-Content -Path $Out -Value ($md -join "`r`n") -Encoding utf8

Write-Host ""
Write-Host "Done. Metrics written to:" -ForegroundColor Green
Write-Host "  $Out" -ForegroundColor Green
Write-Host "Paste that file's contents back into the conversation for evaluation." -ForegroundColor Green
