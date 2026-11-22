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
#    (schema, identity fields, wrapper-exit binding) BEFORE any success is
#    logged, and a failed write/readback fails the wrapper truthfully.
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
    Write-Output $line
}

# Publish the artifact dir as a step output before anything can fail, so the
# upload step can always find the evidence. Path is pure ASCII on the runner.
function Set-StepOutput {
    param([string]$Name, [string]$Value)
    if ($env:GITHUB_OUTPUT) {
        Add-Content -LiteralPath $env:GITHUB_OUTPUT -Value ($Name + '=' + $Value) -Encoding Ascii
    }
}

# Bounded process observation for the in-box WinPS 5.1 path. Keeps a strong
# script-scope reference to the Process object and pre-caches its native
# handle so the exit code cannot be lost to early handle close/GC; reads the
# exit inside a guarded try. stdout/stderr still stream straight into the
# artifact files via redirect handles - this parent buffers nothing, so the
# child's own live evidence survives a kill or crash. Returns exactly one of:
#   EXITED           exitCode is a real [int] read from the native handle
#   TIMEOUT          bound exceeded; process killed and reaped
#   EXIT_UNAVAILABLE process exited but its code could not be read
#                    (observation error - NOT a compiler/child failure)
#   NO_PROCESS_OBJECT Start-Process returned no process object at all
function Invoke-BoundedProcess {
    param([string]$FilePath, [string]$Arguments, [string]$StdoutPath, [string]$StderrPath, [int]$BoundMs, [string]$Label)
    $p = Start-Process -FilePath $FilePath -ArgumentList $Arguments -RedirectStandardOutput $StdoutPath -RedirectStandardError $StderrPath -NoNewWindow -PassThru
    if ($null -eq $p) {
        return @{ kind = 'NO_PROCESS_OBJECT'; exitCode = $null; timedOut = $false }
    }
    try { $null = $p.Handle } catch { Write-Log ($Label + ': native handle pre-cache unavailable: ' + $_.Exception.Message) }
    $script:ObservedProcess = $p
    $timedOut = -not $p.WaitForExit($BoundMs)
    if ($timedOut) {
        try { $p.Kill() } catch { Write-Log ($Label + ': kill failed: ' + $_.Exception.Message) }
        if (-not $p.WaitForExit(15000)) { Write-Log ($Label + ': process not reaped within 15s of kill') }
        return @{ kind = 'TIMEOUT'; exitCode = $null; timedOut = $true }
    }
    $code = $null
    try { $code = $p.ExitCode } catch { $code = $null }
    if ($null -eq $code) {
        return @{ kind = 'EXIT_UNAVAILABLE'; exitCode = $null; timedOut = $false }
    }
    return @{ kind = 'EXITED'; exitCode = [int]$code; timedOut = $false }
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
    if ($compileResult.kind -eq 'EXITED') {
        $compileExitCode = $compileResult.exitCode
        Write-Log ("compile exit=" + $compileExitCode)
    } elseif ($compileResult.kind -eq 'TIMEOUT') {
        Write-Log 'compile observation: TIMEOUT - killed and reaped, raw streams preserved'
    } else {
        Write-Log ("compile observation error: " + $compileResult.kind + " - native exit code unavailable; NOT treated as compiler failure or as success")
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
        if ($childResult.kind -eq 'EXITED') {
            $childExitCode = $childResult.exitCode
            Write-Log ("child exit=" + $childExitCode + " timed_out=False")
            if ($childExitCode -ne 0) { $wrapperExit = 4 } else { $wrapperExit = 0 }
        } elseif ($childResult.kind -eq 'TIMEOUT') {
            $childTimedOut = $true
            Write-Log 'child observation: TIMEOUT after 60s - killed and reaped, events/stdout/stderr preserved'
            $wrapperExit = 3
        } else {
            Write-Log ("child observation error: " + $childResult.kind + " - native exit code unavailable; NOT treated as child failure or as success")
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
    try {
        if ($sandboxCreated) {
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

    # Manifest: distinct data variable ($manifestData) vs the exact artifact
    # path ($script:ManifestPath). The same file is read back and verified
    # before any success is logged; failure here fails the wrapper (exit 9)
    # instead of claiming a write that never landed.
    try {
        $volumeInfo = $null
        if ($null -ne $drive) {
            $volumeInfo = @{ root = $drive.Name; filesystem = $drive.DriveFormat; type = ("{0}" -f $drive.DriveType) }
        }
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
            compileObservation    = $(if ($null -ne $compileResult) { $compileResult.kind } else { 'NOT_RUN' })
            childObservation      = $(if ($null -ne $childResult) { $childResult.kind } else { 'NOT_RUN' })
            childTimedOut         = $childTimedOut
            sandbox               = @{ path = $script:Sandbox; created = $sandboxCreated; absentAfterCleanup = $sandboxAbsent }
            cleanupErrors         = $cleanupErrors
            renameReturns         = $renameLines
            caseResults           = $caseLines
            childHerald           = $caseSummary
            eventsFile            = @{ path = $script:EventsFile; bytes = $eventsBytes }
            streams               = @{ stdout = $script:ChildOut; stderr = $script:ChildErr; compileStdout = $script:CompileOut; compileStderr = $script:CompileErr }
            verdict               = $verdict
        }
        $manifestJson = $manifestData | ConvertTo-Json -Depth 5
        [System.IO.File]::WriteAllText($script:ManifestPath, $manifestJson + "`n", (New-Object System.Text.UTF8Encoding($false)))
        # Read back THE SAME artifact file and verify identity before any
        # success claim. A parse failure or mismatch throws into the catch
        # below: the wrapper then exits 9 and never logs success.
        $readBack = Get-Content -LiteralPath $script:ManifestPath -Raw -Encoding UTF8 | ConvertFrom-Json
        if ($null -eq $readBack) { throw 'manifest readback parsed to null' }
        if ([string]$readBack.schema -ne 'm3-native-rename-proof/1') { throw ('manifest readback schema mismatch: ' + [string]$readBack.schema) }
        if ([string]::IsNullOrWhiteSpace([string]$readBack.generatedUtc)) { throw 'manifest readback missing generatedUtc' }
        if ($null -eq $readBack.verdict) { throw 'manifest readback missing verdict' }
        if ($null -eq $readBack.exitCodes) { throw 'manifest readback missing exitCodes' }
        if ($null -eq $readBack.exitCodes.wrapper) { throw 'manifest readback missing exitCodes.wrapper' }
        if ([int]$readBack.exitCodes.wrapper -ne $wrapperExit) { throw ('manifest readback wrapper-exit mismatch: file=' + [string]$readBack.exitCodes.wrapper + ' actual=' + $wrapperExit) }
        Write-Log ("manifest written+verified: " + $script:ManifestPath + " verdict=" + $verdict)
    }
    catch {
        Write-Log ("MANIFEST_ERROR " + $_.Exception.GetType().Name + ": " + $_.Exception.Message)
        $wrapperExit = 9
        $verdict = 'MANIFEST_UNVERIFIED'
    }
}
exit $wrapperExit
