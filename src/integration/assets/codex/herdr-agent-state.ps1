# installed by herdr
# safe to edit. this hook only activates inside herdr-managed panes.
# HERDR_INTEGRATION_ID=codex
# HERDR_INTEGRATION_VERSION=4

$ErrorActionPreference = 'Stop'

# Read positional args via `$args` rather than a `param()` block so PowerShell
# does not treat `--self-test` as a named parameter binding.
$Action = if ($args.Count -ge 1) { [string]$args[0] } else { '' }

# --self-test is the smoke verification used by goal 7 ci: the script must
# parse cleanly, and exiting 0 here proves PowerShell can execute the file
# end-to-end on the host.
if ($Action -eq '--self-test') {
    Write-Output 'herdr-codex-ps1-ok'
    exit 0
}

# Same gate as the .sh sibling: only the four known actions trigger a
# server roundtrip; anything else is silently ignored.
if (-not @('working', 'idle', 'blocked', 'release').Contains($Action)) {
    exit 0
}

# Drain stdin (Codex pipes the hook payload in as JSON).
$hookInputText = ''
try {
    if ([Console]::IsInputRedirected) {
        $hookInputText = [Console]::In.ReadToEnd()
    }
}
catch {}

if ($env:HERDR_ENV -ne '1') { exit 0 }
if (-not $env:HERDR_SOCKET_PATH) { exit 0 }
if (-not $env:HERDR_PANE_ID) { exit 0 }

$hookInput = $null
if ($hookInputText.Trim()) {
    try { $hookInput = $hookInputText | ConvertFrom-Json -ErrorAction Stop } catch { $hookInput = $null }
}

$sessionId = $null
if ($hookInput -and $hookInput.PSObject.Properties['session_id']) {
    $sid = $hookInput.session_id
    if ($sid -is [string] -and $sid) { $sessionId = $sid }
}

$source = 'herdr:codex'
$unixMs = [int64]([datetime]::UtcNow - [datetime]'1970-01-01Z').TotalMilliseconds
$reportSeq = [int64][datetime]::UtcNow.Ticks * 100
$requestId = "{0}:{1}:{2:000000}" -f $source, $unixMs, (Get-Random -Maximum 1000000)

if ($Action -eq 'release') {
    $params = [ordered]@{
        pane_id = $env:HERDR_PANE_ID
        source  = $source
        agent   = 'codex'
        seq     = $reportSeq
    }
    $method = 'pane.release_agent'
}
else {
    $params = [ordered]@{
        pane_id = $env:HERDR_PANE_ID
        source  = $source
        agent   = 'codex'
        state   = $Action
        seq     = $reportSeq
    }
    if ($sessionId) { $params.agent_session_id = $sessionId }
    $method = 'pane.report_agent'
}

$request = [ordered]@{
    id     = $requestId
    method = $method
    params = $params
} | ConvertTo-Json -Compress -Depth 8

$socketPath = $env:HERDR_SOCKET_PATH
try {
    if ($socketPath -match '^\\\\\.\\pipe\\(.+)$') {
        $pipeName = $matches[1]
        $client = New-Object System.IO.Pipes.NamedPipeClientStream('.', $pipeName, 'InOut')
        try {
            $client.Connect(500)
            $writer = New-Object System.IO.StreamWriter($client, [System.Text.Encoding]::UTF8)
            $writer.NewLine = "`n"
            $writer.AutoFlush = $true
            $writer.WriteLine($request)
            try {
                $client.ReadTimeout = 500
                $reader = New-Object System.IO.StreamReader($client, [System.Text.Encoding]::UTF8)
                $buf = New-Object char[] 4096
                $null = $reader.Read($buf, 0, $buf.Length)
            }
            catch {}
        }
        finally {
            $client.Dispose()
        }
    }
}
catch {
    # Best-effort, silent failure parity with the python sibling.
}

exit 0
