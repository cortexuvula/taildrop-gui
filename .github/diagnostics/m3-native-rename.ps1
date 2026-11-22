# M3 native-rename proof: diagnostic-only wrapper for hosted Windows CI.
# Compiles .github/diagnostics/m3-native-rename.cs with the runner's in-box
# .NET Framework csc (no installs, no downloads), runs it against a fresh
# GUID sandbox created directly under the resolved TEMP, bounds the child to
# 60 seconds, and preserves every raw stream in one fixed artifact directory.
# The child writes its own events file line-by-line as events occur, and its
# stdout/stderr are redirected straight into files, so evidence survives an
# interrupted run. No secrets, no network calls, no product code, and no
# shell command is built from user input.
#
# R3 repairs (evidence: hosted run 45078764946):
#  - Start-Process -PassThru + WaitForExit returned a NULL ExitCode on the
#    hosted WinPS 5.1 path (wrapper.log line "compile exit=" logged blank)
#    even though csc had produced the exe. All process observation now goes
#    through Invoke-BoundedProcess: strong script-scope reference, cached
#    native handle, exit read inside a guarded try. An unreadable exit is
#    classified as an OBSERVATION ERROR - never as success, never as a
#    compiler/child failure, never silently blank.
#  - $script:Manifest (path) collided case-insensitively with the $manifest
#    data object (one script variable), so the manifest write did not land
#    in manifest.json (0 bytes in the artifact) while the log still claimed
#    success. The path is now $script:ManifestPath and the data is
#    $manifestData; the exact artifact file is read back and verified
#    BEFORE any success is logged, and a failed write/readback fails the
#    wrapper truthfully.
#
# R4 repairs (Codie R3-F1/R3-F2):
#  - Invoke-BoundedProcess now RETURNS the actual bounded reap result, kill
#    and reap errors, the wait failure, and the confirmed-termination state
#    (stopped). A true bounded wait confirms termination even when the exit
#    code cannot be read; a false/exception reap after kill does NOT. Both
#    callers log and serialize that record - "killed and reaped" is never
#    claimed unconditionally. When the child's termination is unconfirmed,
#    destructive GUID-sandbox cleanup is DEFERRED (sandbox_absent stays
#    unknown, exact path + process identity retained, verdict gains
#    +CLEANUP_DEFERRED); the normal completed-control cleanup path is
#    unchanged. All waits remain bounded; no retries, sleeps, unbounded
#    reaps, or unrelated process kills.
#  - Manifest readback verifies expected identity AND content: exact JSON
#    text equality plus parsed comparison of runId, repository,
#    generatedUtc and verdict against this invocation's manifestData
#    (GitHub identity must be nonempty when the CI environment provides
#    it), exit fields bound to the just-serialized values including
#    legitimate nulls (NOT_RUN/unknown), and process-observation kind/
#    stopped bindings. Any mismatch fails BEFORE 'manifest written+verified'.
#
# R5 repairs (Codie R4-F1/R4-F2):
#  - PowerShell -eq/-ne are case-insensitive, so every readback equality
#    that must be EXACT (JSON text, schema, identity fields, verdict,
#    observation kinds, NOT_RUN) now uses [String]::Equals with
#    StringComparison.Ordinal. Bool/int/null checks and legitimate
#    null/NOT_RUN acceptance are unchanged; this wrapper performs no
#    filesystem-path string comparisons (the C# child owns those,
#    intentionally case-insensitive).
#  - Write-Log writes via [Console]::Out.WriteLine (file log retained), so
#    it never emits a pipeline object: Invoke-BoundedProcess returns
#    exactly one observation hashtable on every supported branch, including
#    the Handle-precache-error path, and the typed formatter always binds.
#
# Exit codes: 0 completed and internally consistent (diagnostic PASS - this
# is NOT product-test green); 2 compile failed; 3 child exceeded 60s;
# 4 child exit nonzero; 5 sandbox not absent after cleanup; 6 wrapper error;
# 7 compile exceeded its bound; 8 process exit unobservable (compile or
# child); 9 manifest not written/verified.

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

$script:TempRoot    = [System.IO.Path]::GetTempPath()
$script:ArtifactDir = Join-Path $script:TempRoot 'm3-native-rename-proof-out'
$script:WrapperLog  = Join-Path $script:ArtifactDir 'wrapper.log'
$script:CompileOut  = Join-Path $script:ArtifactDir 'compile-stdout.txt'
$script:CompileErr  = Join-Path $script:ArtifactDir 'compile-stderr.txt'
$script:ChildOut    = Join-Path $script:ArtifactDir 'child-stdout.txt'
$script:ChildErr    = Join-Path $script:ArtifactDir 'child-stderr.txt'
$script:EventsFile  = Join-Path $script:ArtifactDir 'child-events.log'
$script:ManifestPath = Join-Path $script:ArtifactDir 'manifest.json'
$script:ObservedProcess = $null

function Write-Log {
    param([string]$Message)
    $line = ('{0:O} wrapper {1}' -f [DateTime]::UtcNow, $Message)
    Add-Content -LiteralPath $script:WrapperLog -Value $line -Encoding Ascii
    # R4-F2: console, NOT the success stream. Write-Output here would join
    # the caller's function result (Invoke-BoundedProcess would hand back a
    # mixed string+hashtable Object[] that cannot bind to the typed
    # formatter). [Console]::Out.WriteLine still surfaces in the CI job log
    # but emits no pipeline object.
    [Console]::Out.WriteLine($line)
}

# Publish the artifact dir as a step output before anything can fail, so the
# upload step can always find the evidence. Path is pure ASCII on the runner.
function Set-StepOutput {
    param([string]$Name, [string]$Value)
    if ($env:GITHUB_OUTPUT) {
        Add-Content -LiteralPath $env:GITHUB_OUTPUT -Value ($Name + '=' + $Value) -Encoding Ascii
    }
}

# Renders the full returned observation record for logs. Every field that is
# unknown is serialized as null - nothing is guessed.
function Format-Observation {
    param([hashtable]$R)
    $parts = @('kind=' + [string]$R.kind)
    $parts += ('terminated=' + [string]$R.stopped)
    $parts += ('waitTrue=' + [string]$R.waitTrue)
    if ($null -ne $R.exitCode) { $parts += ('exit=' + [string]$R.exitCode) }
    if ($null -ne $R.procId) { $parts += ('procId=' + [string]$R.procId) }
    if ($null -ne $R.processName) { $parts += ('name=' + [string]$R.processName) }
    if ($null -ne $R.killError) { $parts += ('killError=' + [string]$R.killError) }
    if ($null -ne $R.reapError) { $parts += ('reapError=' + [string]$R.reapError) }
    if ($null -ne $R.waitError) { $parts += ('waitError=' + [string]$R.waitError) }
    return [string]::Join(' ', $parts)
}

# Bounded process observation for the in-box WinPS 5.1 path. Keeps a strong
# script-scope reference to the Process object and pre-caches its native
# handle so the exit code cannot be lost to early handle close/GC. stdout and
# stderr still stream straight into the artifact files via redirect handles -
# this parent buffers nothing, so the child's own live evidence survives a
# kill or crash. Returns a record with EXACTLY the observed facts:
#   kind           EXITED | TIMEOUT | WAIT_ERROR | EXIT_UNAVAILABLE | NO_PROCESS_OBJECT
#   exitCode       [int] when read from the native handle, else $null
#   waitTrue       the bounded WaitForExit result ($true = termination observed)
#   stopped        termination CONFIRMED: true only when the bounded wait
#                  returned true (directly, or as the reap after kill).
#                  A false/exception reap after kill stays $false: unknown.
#   killError/reapError/waitError  error text or $null, never swallowed
#   procId/processName             process identity, $null when unreadable
function Invoke-BoundedProcess {
    param([string]$FilePath, [string]$Arguments, [string]$StdoutPath, [string]$StderrPath, [int]$BoundMs, [string]$Label)
    $p = Start-Process -FilePath $FilePath -ArgumentList $Arguments -RedirectStandardOutput $StdoutPath -RedirectStandardError $StderrPath -NoNewWindow -PassThru
    if ($null -eq $p) {
        return @{ kind = 'NO_PROCESS_OBJECT'; exitCode = $null; waitTrue = $false; stopped = $false; killError = $null; reapError = $null; waitError = $null; procId = $null; processName = $null }
    }
    $procId = $null
    $procName = $null
    try { $procId = [int]$p.Id } catch { }   # identity is best-effort; null is serialized honestly
    try { $procName = [string]$p.ProcessName } catch { }
    try { $null = $p.Handle } catch { Write-Log ($Label + ': native handle pre-cache unavailable: ' + $_.Exception.Message) }
    $script:ObservedProcess = $p
    $waitTrue = $false
    $waitError = $null
    try { $waitTrue = $p.WaitForExit($BoundMs) } catch { $waitError = $_.Exception.Message }
    if ($waitTrue) {
        # Termination IS confirmed by the bounded wait itself, even when the
        # exit code cannot be read afterwards.
        $code = $null
        try { $code = $p.ExitCode } catch { $code = $null }
        if ($null -ne $code) {
            return @{ kind = 'EXITED'; exitCode = [int]$code; waitTrue = $true; stopped = $true; killError = $null; reapError = $null; waitError = $null; procId = $procId; processName = $procName }
        }
        return @{ kind = 'EXIT_UNAVAILABLE'; exitCode = $null; waitTrue = $true; stopped = $true; killError = $null; reapError = $null; waitError = $waitError; procId = $procId; processName = $procName }
    }
    # Termination NOT confirmed within the bound (timeout, or the wait itself
    # failed): one bounded kill, one bounded reap. stopped reflects ONLY the
    # reap result - a false or throwing reap leaves it $false (unknown), and
    # no caller may claim the process was killed and reaped.
    $killError = $null
    try { $p.Kill() } catch { $killError = $_.Exception.Message }
    $reapTrue = $false
    $reapError = $null
    try { $reapTrue = $p.WaitForExit(15000) } catch { $reapError = $_.Exception.Message }
    $kind = 'TIMEOUT'
    if ($null -ne $waitError) { $kind = 'WAIT_ERROR' }
    return @{ kind = $kind; exitCode = $null; waitTrue = $false; stopped = [bool]$reapTrue; killError = $killError; reapError = $reapError; waitError = $waitError; procId = $procId; processName = $procName }
}

$wrapperExit   = 6
$compileArgvLine = $null
$executeArgvLine = $null
$compileExitCode = $null
$childExitCode   = $null
$childTimedOut   = $false
$csDigest  = $null
$exeDigest = $null
$cscPath   = $null
$cscVersion = $null
$drive     = $null
$sandboxCreated = $false
$compileResult = $null
$childResult = $null

try {
    New-Item -ItemType Directory -Force -Path $script:ArtifactDir | Out-Null
    # Truncate every evidence file so a local rerun cannot append stale events.
    foreach ($f in @($script:WrapperLog, $script:CompileOut, $script:CompileErr,
                     $script:ChildOut, $script:ChildErr, $script:EventsFile, $script:ManifestPath)) {
        [System.IO.File]::WriteAllText($f, '', [System.Text.Encoding]::ASCII)
    }
    Set-StepOutput 'artifact-dir' $script:ArtifactDir

    $drive = New-Object System.IO.DriveInfo($script:TempRoot)
    Write-Log ("powershell={0} clr={1}" -f $PSVersionTable.PSVersion, [Environment]::Version)
    Write-Log ("os={0} arch={1} temp_root={2}" -f [Environment]::OSVersion.VersionString, $env:PROCESSOR_ARCHITECTURE, $script:TempRoot)
    Write-Log ("volume root={0} filesystem={1} type={2} ready={3}" -f $drive.Name, $drive.DriveFormat, $drive.DriveType, $drive.IsReady)

    $cscPath = Join-Path $env:WINDIR 'Microsoft.NET\Framework64\v4.0.30319\csc.exe'
    if (-not (Test-Path -LiteralPath $cscPath)) {
        $cscPath = Join-Path $env:WINDIR 'Microsoft.NET\Framework\v4.0.30319\csc.exe'
    }
    if (-not (Test-Path -LiteralPath $cscPath)) { throw ("no in-box .NET Framework csc under " + $env:WINDIR) }
    $cscVersion = (Get-Item -LiteralPath $cscPath).VersionInfo.FileVersion
    Write-Log ("csc={0} file_version={1}" -f $cscPath, $cscVersion)

    $cs = Join-Path $PSScriptRoot 'm3-native-rename.cs'
    if (-not (Test-Path -LiteralPath $cs)) { throw ("diagnostic source missing: " + $cs) }
    $exe = Join-Path $script:ArtifactDir 'm3-native-rename-child.exe'

    $script:Sandbox = Join-Path $script:TempRoot ('m3-native-rename-' + [Guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory -Path $script:Sandbox | Out-Null
    $sandboxCreated = $true
    Write-Log ("sandbox=" + $script:Sandbox)

    $compileArgvLine = ('/nologo /warn:4 /target:exe /out:"' + $exe + '" "' + $cs + '"')
    Write-Log ("compile argv: csc " + $compileArgvLine)
    $compileResult = Invoke-BoundedProcess -FilePath $cscPath -Arguments $compileArgvLine -StdoutPath $script:CompileOut -StderrPath $script:CompileErr -BoundMs 120000 -Label 'csc'
    $compileObsText = Format-Observation $compileResult
    if ($compileResult.kind -eq 'EXITED') {
        $compileExitCode = $compileResult.exitCode
        Write-Log ("compile exit=" + $compileExitCode + " (termination observed) [" + $compileObsText + "]")
    } elseif ($compileResult.stopped) {
        Write-Log ("compile observation: " + $compileResult.kind + " - termination confirmed (bounded wait true); exit code unavailable; NOT success and NOT compiler failure [" + $compileObsText + "]")
    } else {
        Write-Log ("compile observation: " + $compileResult.kind + " - termination NOT confirmed - process may still be running; kill/reap errors recorded; no reap claim made [" + $compileObsText + "]")
    }

    if ($compileResult.kind -eq 'TIMEOUT') {
        $wrapperExit = 7
    } elseif ($compileResult.kind -ne 'EXITED') {
        $wrapperExit = 8
    } elseif ($compileExitCode -ne 0) {
        # Raw compiler output is already persisted; never run a stale exe.
        $wrapperExit = 2
    } else {
        $csDigest  = (Get-FileHash -LiteralPath $cs -Algorithm SHA256).Hash
        $exeDigest = (Get-FileHash -LiteralPath $exe -Algorithm SHA256).Hash
        Write-Log ("sha256 cs=" + $csDigest + " exe=" + $exeDigest)

        $executeArgvLine = ('"' + $exe + '" "' + $script:Sandbox + '" "' + $script:EventsFile + '"')
        Write-Log ("execute argv: " + $executeArgvLine + " (bound 60s)")
        $childResult = Invoke-BoundedProcess -FilePath $exe -Arguments ('"' + $script:Sandbox + '" "' + $script:EventsFile + '"') -StdoutPath $script:ChildOut -StderrPath $script:ChildErr -BoundMs 60000 -Label 'child'
        $childObsText = Format-Observation $childResult
        if ($childResult.kind -eq 'EXITED') {
            $childExitCode = $childResult.exitCode
            Write-Log ("child exit=" + $childExitCode + " timed_out=False (termination observed) [" + $childObsText + "]")
            if ($childExitCode -ne 0) { $wrapperExit = 4 } else { $wrapperExit = 0 }
        } elseif ($childResult.kind -eq 'TIMEOUT') {
            $childTimedOut = $true
            if ($childResult.stopped) {
                Write-Log ("child observation: TIMEOUT after 60s - termination confirmed after bounded kill+reap (exit code unknown; NOT child success or failure) [" + $childObsText + "]")
            } else {
                Write-Log ("child observation: TIMEOUT after 60s - termination NOT confirmed - child may still be running; events/stdout/stderr preserved as-is [" + $childObsText + "]")
            }
            $wrapperExit = 3
        } elseif ($childResult.stopped) {
            Write-Log ("child observation: " + $childResult.kind + " - termination confirmed (bounded wait true); exit code unavailable; NOT child success or failure [" + $childObsText + "]")
            $wrapperExit = 8
        } else {
            Write-Log ("child observation: " + $childResult.kind + " - termination NOT confirmed - child may still be running; no reap claim made [" + $childObsText + "]")
            $wrapperExit = 8
        }
    }
}
catch {
    Write-Log ("WRAPPER_ERROR " + $_.Exception.GetType().Name + ": " + $_.Exception.Message)
    $wrapperExit = 6
}
finally {
    # Sandbox readback and cleanup failures never erase the evidence logs.
    $cleanupErrors = @()
    $sandboxAbsent = $true
    # R3-F1: while our own child may still be running inside the GUID
    # sandbox, destructive cleanup is DEFERRED - never raced. Deferral keeps
    # the exact path and process identity, marks the run incomplete (the
    # wrapper exit is already 3/8 on these paths), and makes NO absence
    # claim. Only a confirmed-stopped child (or a child never launched)
    # reaches the normal bounded cleanup, unchanged from before.
    $cleanupDeferred = $false
    $deferredReason = $null
    $childTerminationConfirmed = ($null -eq $childResult) -or ($childResult.stopped -eq $true)
    try {
        if ($sandboxCreated -and (-not $childTerminationConfirmed)) {
            $cleanupDeferred = $true
            $deferredReason = 'child termination unconfirmed - destructive sandbox cleanup deferred; artifacts possibly unstable'
            $sandboxAbsent = $null
            $cleanupErrors += ('destructive cleanup deferred: ' + $script:Sandbox)
            Write-Log ('CLEANUP_DEFERRED: ' + $deferredReason)
            Write-Log ('deferred sandbox path: ' + $script:Sandbox)
            if ($null -ne $childResult -and $null -ne $childResult.procId) { Write-Log ('unreaped child procId=' + $childResult.procId + ' name=' + [string]$childResult.processName) }
        }
        if ($sandboxCreated) {
            if ($cleanupDeferred) {
                Write-Log 'sandbox_absent=unknown (destructive cleanup deferred: child termination unconfirmed)'
            } else {
                $sandboxAbsent = -not (Test-Path -LiteralPath $script:Sandbox)
                if (-not $sandboxAbsent) {
                    Write-Log 'sandbox still present after child - removing leftovers'
                    try { Remove-Item -LiteralPath $script:Sandbox -Recurse -Force -ErrorAction Stop }
                    catch { $cleanupErrors += ("remove sandbox: " + $_.Exception.Message) }
                    $sandboxAbsent = -not (Test-Path -LiteralPath $script:Sandbox)
                }
                Write-Log ("sandbox_absent=" + $sandboxAbsent)
                if (-not $sandboxAbsent) {
                    $cleanupErrors += ('sandbox present after forced cleanup: ' + $script:Sandbox)
                    if ($wrapperExit -lt 5) { $wrapperExit = 5 }
                }
            }
        } else {
            Write-Log 'sandbox was never created'
        }
    }
    catch {
        $cleanupErrors += ("readback: " + $_.Exception.Message)
        if ($wrapperExit -lt 5) { $wrapperExit = 5 }
    }

    $caseLines    = @()
    $renameLines  = @()
    $eventsBytes  = 0
    $caseSummary  = @()
    try {
        if (Test-Path -LiteralPath $script:EventsFile) {
            $eventsBytes = (Get-Item -LiteralPath $script:EventsFile).Length
            $caseLines   = @(Get-Content -LiteralPath $script:EventsFile | Where-Object { $_ -match "`tCASE_RESULT`t" })
            $renameLines = @(Get-Content -LiteralPath $script:EventsFile | Where-Object { $_ -match "`tRENAME_RETURN`t" })
            $caseSummary = @(Get-Content -LiteralPath $script:EventsFile | Where-Object { $_ -match "`tHARNESS_EXIT`t" })
        }
    }
    catch {
        $cleanupErrors += ("events readback: " + $_.Exception.Message)
    }

    $verdict = 'INCOMPLETE'
    switch ($wrapperExit) {
        0 { $verdict = 'PASS-diagnostic-only' }
        2 { $verdict = 'COMPILE_FAILED' }
        3 { $verdict = 'CHILD_TIMEOUT' }
        4 { $verdict = 'CHILD_NONZERO' }
        5 { $verdict = 'SANDBOX_PRESENT' }
        6 { $verdict = 'WRAPPER_ERROR' }
        7 { $verdict = 'COMPILE_TIMEOUT' }
        8 { $verdict = 'EXIT_UNAVAILABLE' }
        9 { $verdict = 'MANIFEST_UNVERIFIED' }
    }
    # Safety cleanup deferral is distinguished from the base verdict rather
    # than replacing it (a helper failure stays its own verdict).
    if ($cleanupDeferred) { $verdict = $verdict + '+CLEANUP_DEFERRED' }

    # Manifest: distinct data variable ($manifestData) vs the exact artifact
    # path ($script:ManifestPath). The same file is read back and verified
    # for expected identity AND content before any success is logged; failure
    # here fails the wrapper (exit 9) instead of claiming a write that never
    # landed. Process observations serialize the full observed record,
    # including legitimate nulls for NOT_RUN/unknown.
    try {
        $volumeInfo = $null
        if ($null -ne $drive) {
            $volumeInfo = @{ root = $drive.Name; filesystem = $drive.DriveFormat; type = ("{0}" -f $drive.DriveType) }
        }
        $notRun = @{ kind = 'NOT_RUN'; exitCode = $null; waitTrue = $null; stopped = $null; killError = $null; reapError = $null; waitError = $null; procId = $null; processName = $null }
        $manifestData = [ordered]@{
            schema                = 'm3-native-rename-proof/1'
            note                  = 'Diagnostic measurement of the vendored legacy rename only; PASS here is NOT product-test green.'
            generatedUtc          = ([DateTime]::UtcNow.ToString('o'))
            runId                 = $env:GITHUB_RUN_ID
            repository            = $env:GITHUB_REPOSITORY
            osVersion             = ([Environment]::OSVersion.VersionString)
            processorArchitecture = $env:PROCESSOR_ARCHITECTURE
            powershellVersion     = ($PSVersionTable.PSVersion.ToString())
            tempRoot              = $script:TempRoot
            volume                = $volumeInfo
            csc                   = @{ path = $cscPath; fileVersion = $cscVersion }
            compileArgv           = $compileArgvLine
            executeArgv           = $executeArgvLine
            boundsMs              = @{ compile = 120000; child = 60000 }
            digests               = @{ cs = $csDigest; exe = $exeDigest }
            exitCodes             = @{ compile = $compileExitCode; child = $childExitCode; wrapper = $wrapperExit }
            processObservation    = @{
                compile = $(if ($null -ne $compileResult) { $compileResult } else { $notRun })
                child   = $(if ($null -ne $childResult) { $childResult } else { $notRun })
            }
            childTimedOut         = $childTimedOut
            sandbox               = @{ path = $script:Sandbox; created = $sandboxCreated; absentAfterCleanup = $sandboxAbsent; destructiveCleanupDeferred = $cleanupDeferred; deferredReason = $deferredReason; childProcId = $(if ($null -ne $childResult) { $childResult.procId } else { $null }) }
            cleanupErrors         = $cleanupErrors
            renameReturns         = $renameLines
            caseResults           = $caseLines
            childHerald           = $caseSummary
            eventsFile            = @{ path = $script:EventsFile; bytes = $eventsBytes }
            streams               = @{ stdout = $script:ChildOut; stderr = $script:ChildErr; compileStdout = $script:CompileOut; compileStderr = $script:CompileErr }
            verdict               = $verdict
        }
        $manifestJson = $manifestData | ConvertTo-Json -Depth 5
        $manifestText = $manifestJson + "`n"
        [System.IO.File]::WriteAllText($script:ManifestPath, $manifestText, (New-Object System.Text.UTF8Encoding($false)))
        # R3-F2: read back the exact artifact file and verify expected
        # identity AND content BEFORE any success claim. First the written
        # text must equal the intended serialization byte-for-byte, then the
        # parsed file must match THIS invocation's identity (runId,
        # repository, generatedUtc, verdict - GitHub identity nonempty when
        # the environment provides it), bind the exit fields to the
        # just-serialized values (null included), and bind the process
        # observation records (kind + stopped). A parse failure, mismatch or
        # contradiction throws into the catch below: exit 9, never success.
        $readText = [System.IO.File]::ReadAllText($script:ManifestPath, [System.Text.Encoding]::UTF8)
        # R4-F1: ORDINAL text equality. PowerShell -eq/-ne are case-insensitive,
        # so a case-only corruption of the written text would compare equal;
        # [String]::Equals with StringComparison.Ordinal is exact.
        if (-not [String]::Equals($readText, $manifestText, [StringComparison]::Ordinal)) { throw 'manifest readback text mismatch: artifact differs from intended serialization' }
        $readBack = $readText | ConvertFrom-Json
        if ($null -eq $readBack) { throw 'manifest readback parsed to null' }
        if (-not [String]::Equals([string]$readBack.schema, 'm3-native-rename-proof/1', [StringComparison]::Ordinal)) { throw ('manifest readback schema mismatch: ' + [string]$readBack.schema) }
        if ($env:GITHUB_RUN_ID -and [string]::IsNullOrWhiteSpace([string]$manifestData.runId)) { throw 'GitHub run identity missing from manifest' }
        if ($env:GITHUB_REPOSITORY -and [string]::IsNullOrWhiteSpace([string]$manifestData.repository)) { throw 'GitHub repository identity missing from manifest' }
        foreach ($field in @('runId', 'repository', 'generatedUtc', 'verdict')) {
            $expectedValue = [string]$manifestData[$field]
            $actualValue = [string]$readBack.$field
            # R4-F1: ordinal comparison - serialized identity/verdict strings
            # must match exactly, not case-insensitively.
            if (-not [String]::Equals($actualValue, $expectedValue, [StringComparison]::Ordinal)) { throw ('manifest readback ' + $field + ' mismatch: expected=[' + $expectedValue + '] actual=[' + $actualValue + ']') }
        }
        if ([string]::IsNullOrWhiteSpace([string]$readBack.generatedUtc)) { throw 'manifest readback generatedUtc empty' }
        if ([string]::IsNullOrWhiteSpace([string]$readBack.verdict)) { throw 'manifest readback verdict empty' }
        if ($null -eq $readBack.exitCodes) { throw 'manifest readback missing exitCodes' }
        if ($null -eq $readBack.exitCodes.wrapper) { throw 'manifest readback missing exitCodes.wrapper' }
        if ([int]$readBack.exitCodes.wrapper -ne $wrapperExit) { throw ('manifest readback wrapper-exit mismatch: file=' + [string]$readBack.exitCodes.wrapper + ' actual=' + $wrapperExit) }
        if ($null -eq $compileExitCode) {
            if ($null -ne $readBack.exitCodes.compile) { throw 'manifest readback compile exit not null as serialized' }
        } elseif ([int]$readBack.exitCodes.compile -ne [int]$compileExitCode) { throw 'manifest readback compile exit mismatch' }
        if ($null -eq $childExitCode) {
            if ($null -ne $readBack.exitCodes.child) { throw 'manifest readback child exit not null as serialized' }
        } elseif ([int]$readBack.exitCodes.child -ne [int]$childExitCode) { throw 'manifest readback child exit mismatch' }
        if ($null -eq $readBack.processObservation) { throw 'manifest readback missing processObservation' }
        if ($null -ne $compileResult) {
            if (-not [String]::Equals([string]$readBack.processObservation.compile.kind, [string]$compileResult.kind, [StringComparison]::Ordinal)) { throw 'manifest readback compile observation kind mismatch' }
            if ([bool]$readBack.processObservation.compile.stopped -ne [bool]$compileResult.stopped) { throw 'manifest readback compile stopped mismatch' }
        } elseif (-not [String]::Equals([string]$readBack.processObservation.compile.kind, 'NOT_RUN', [StringComparison]::Ordinal)) { throw 'manifest readback compile observation must be NOT_RUN' }
        if ($null -ne $childResult) {
            if (-not [String]::Equals([string]$readBack.processObservation.child.kind, [string]$childResult.kind, [StringComparison]::Ordinal)) { throw 'manifest readback child observation kind mismatch' }
            if ([bool]$readBack.processObservation.child.stopped -ne [bool]$childResult.stopped) { throw 'manifest readback child stopped mismatch' }
        } elseif (-not [String]::Equals([string]$readBack.processObservation.child.kind, 'NOT_RUN', [StringComparison]::Ordinal)) { throw 'manifest readback child observation must be NOT_RUN' }
        Write-Log ("manifest written+verified: " + $script:ManifestPath + " verdict=" + $verdict)
    }
    catch {
        Write-Log ("MANIFEST_ERROR " + $_.Exception.GetType().Name + ": " + $_.Exception.Message)
        $wrapperExit = 9
        $verdict = 'MANIFEST_UNVERIFIED'
    }
}
exit $wrapperExit
