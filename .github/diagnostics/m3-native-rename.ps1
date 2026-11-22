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
# Exit codes: 0 completed and internally consistent (diagnostic PASS - this
# is NOT product-test green); 2 compile failed; 3 child exceeded 60s;
# 4 child exit nonzero; 5 sandbox not absent after cleanup; 6 wrapper error.

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
$script:Manifest    = Join-Path $script:ArtifactDir 'manifest.json'

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
$script:Sandbox = $null
$sandboxCreated = $false

try {
    New-Item -ItemType Directory -Force -Path $script:ArtifactDir | Out-Null
    # Truncate every evidence file so a local rerun cannot append stale events.
    foreach ($f in @($script:WrapperLog, $script:CompileOut, $script:CompileErr,
                     $script:ChildOut, $script:ChildErr, $script:EventsFile, $script:Manifest)) {
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
    $cp = Start-Process -FilePath $cscPath -ArgumentList $compileArgvLine -RedirectStandardOutput $script:CompileOut -RedirectStandardError $script:CompileErr -NoNewWindow -PassThru
    if (-not $cp.WaitForExit(120000)) { $cp.Kill(); $cp.WaitForExit(); throw 'csc exceeded its 120s bound' }
    $compileExitCode = $cp.ExitCode
    Write-Log ("compile exit=" + $compileExitCode)

    if ($compileExitCode -ne 0) {
        # Raw compiler output is already persisted; never run a stale exe.
        $wrapperExit = 2
    } else {
        $csDigest  = (Get-FileHash -LiteralPath $cs -Algorithm SHA256).Hash
        $exeDigest = (Get-FileHash -LiteralPath $exe -Algorithm SHA256).Hash
        Write-Log ("sha256 cs=" + $csDigest + " exe=" + $exeDigest)

        $executeArgvLine = ('"' + $exe + '" "' + $script:Sandbox + '" "' + $script:EventsFile + '"')
        Write-Log ("execute argv: " + $executeArgvLine + " (bound 60s)")
        $rp = Start-Process -FilePath $exe -ArgumentList ('"' + $script:Sandbox + '" "' + $script:EventsFile + '"') -RedirectStandardOutput $script:ChildOut -RedirectStandardError $script:ChildErr -NoNewWindow -PassThru
        if (-not $rp.WaitForExit(60000)) {
            $childTimedOut = $true
            Write-Log 'child exceeded 60s bound - killing'
            $rp.Kill()
            $rp.WaitForExit()
        }
        $childExitCode = $rp.ExitCode
        Write-Log ("child exit=" + $childExitCode + " timed_out=" + $childTimedOut)
        if ($childTimedOut) { $wrapperExit = 3 }
        elseif ($childExitCode -ne 0) { $wrapperExit = 4 }
        else { $wrapperExit = 0 }
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
    }

    try {
        $volumeInfo = $null
        if ($null -ne $drive) {
            $volumeInfo = @{ root = $drive.Name; filesystem = $drive.DriveFormat; type = ("{0}" -f $drive.DriveType) }
        }
        $manifest = [ordered]@{
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
        $json = $manifest | ConvertTo-Json -Depth 5
        [System.IO.File]::WriteAllText($script:Manifest, $json + "`n", (New-Object System.Text.UTF8Encoding($false)))
        Write-Log ("manifest written, verdict=" + $verdict)
    }
    catch {
        Write-Log ("MANIFEST_ERROR " + $_.Exception.GetType().Name + ": " + $_.Exception.Message)
        if ($wrapperExit -eq 0) { $wrapperExit = 6 }
    }
}
exit $wrapperExit
