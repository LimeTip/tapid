import { strictEqual as equal, match } from "node:assert/strict";
import { execFile } from "node:child_process";
import { mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";
import { promisify } from "node:util";
import { renderInstallers } from "./bootstrap.ts";
import { TARGETS } from "./release.ts";

const run = promisify(execFile);

test("PowerShell downloads share one deadline across headers, redirects and partial bodies", async (context) => {
  try { await run("pwsh", ["-NoProfile", "-Command", "$null"]); }
  catch { context.skip("PowerShell is unavailable"); return; }
  const directory = await mkdtemp(join(tmpdir(), "tapid-download-deadline-"));
  try {
    // Exercise the template and the actual generated installer, without network access.
    for (const target of TARGETS) {
      await writeFile(join(directory, `tapid-1.2.3-${target}.tar.gz`), "fixture");
    }
    await renderInstallers(directory, "1.2.3", "https://bootstrap.example/v1.2.3");
    const sources = [
      await readFile(new URL("../../scripts/install.ps1", import.meta.url), "utf8"),
      await readFile(join(directory, "install.ps1"), "utf8"),
    ];
    await writeFile(join(directory, "transport.cs"), `
using System;
using System.IO;
using System.Net;
using System.Net.Http;
using System.Threading;
using System.Threading.Tasks;
public sealed class DownloadStream : Stream {
    public bool Disposed;
    public bool Partial;
    public bool Stall;
    public CancellationToken ReadToken;
    public override int Read(byte[] b, int o, int n) { throw new Exception("synchronous body read"); }
    public override async Task<int> ReadAsync(byte[] b, int o, int n, CancellationToken token) {
        ReadToken = token;
        if (!Partial) { Partial = true; b[o] = 42; return 1; }
        if (Stall) await Task.Delay(Timeout.Infinite, token);
        return 0;
    }
    protected override void Dispose(bool disposing) { Disposed = true; base.Dispose(disposing); }
    public override bool CanRead => true;
    public override bool CanSeek => false;
    public override bool CanWrite => false;
    public override long Length => throw new NotSupportedException();
    public override long Position { get => throw new NotSupportedException(); set => throw new NotSupportedException(); }
    public override void Flush() { }
    public override long Seek(long o, SeekOrigin s) { throw new NotSupportedException(); }
    public override void SetLength(long n) { throw new NotSupportedException(); }
    public override void Write(byte[] b, int o, int n) { throw new NotSupportedException(); }
}
public sealed class DownloadHandler : HttpMessageHandler {
    public string Mode;
    public bool Disposed;
    public int Requests;
    public int DisposedResponses;
    public DownloadStream Body = new DownloadStream();
    public DownloadHandler(string mode) { Mode = mode; Body.Stall = mode == "body"; }
    protected override async Task<HttpResponseMessage> SendAsync(HttpRequestMessage request, CancellationToken token) {
        Requests++;
        if (Mode == "headers") await Task.Delay(Timeout.Infinite, token);
        bool redirect = Mode == "redirects" || Mode == "limit" || Mode == "downgrade" || Mode == "missing";
        if (Mode == "redirects") await Task.Delay(40, token);
        var response = new DownloadResponse(this, redirect ? HttpStatusCode.Redirect : HttpStatusCode.OK);
        response.RequestMessage = request;
        if (redirect && Mode != "missing") response.Headers.Location = new Uri(Mode == "downgrade" ? "http://fixture.invalid/next" : "/next", UriKind.RelativeOrAbsolute);
        response.Content = new StreamContent(Body);
        if (Mode == "advertised") response.Content.Headers.ContentLength = 11;
        return response;
    }
    public sealed class DownloadResponse : HttpResponseMessage {
        private DownloadHandler Owner;
        public DownloadResponse(DownloadHandler owner, HttpStatusCode status) : base(status) { Owner = owner; }
        protected override void Dispose(bool disposing) { Owner.DisposedResponses++; base.Dispose(disposing); }
    }
    protected override void Dispose(bool disposing) { Disposed = true; base.Dispose(disposing); }
}
`);
    for (const [index, source] of sources.entries()) {
      const functions = source.slice(source.indexOf("function Save-BoundedHttpsFile"), source.indexOf("function Test-ReleaseVersion"));
      await writeFile(join(directory, "test.ps1"), `$ErrorActionPreference = 'Stop'
Add-Type -Path (Join-Path $PSScriptRoot 'transport.cs')
function Fail([string]$Message) { throw $Message }
${functions}
foreach ($mode in @('body', 'headers', 'redirects', 'success', 'advertised', 'actual', 'limit', 'downgrade', 'missing')) {
    $handler = [DownloadHandler]::new($mode)
    $path = Join-Path $PSScriptRoot 'download'
    $maximum = if ($mode -eq 'actual') { 0 } else { 10 }
    $timeout = if ($mode -in @('body', 'headers', 'redirects')) { [TimeSpan]::FromMilliseconds(200) } else { [TimeSpan]::FromSeconds(5) }
    $watch = [Diagnostics.Stopwatch]::StartNew()
    $errorText = ''
    try { Save-BoundedHttpsFile 'https://fixture.invalid/start' $path $maximum -HttpHandler $handler -Timeout $timeout } catch { $errorText = $_.ToString() }
    if (-not $handler.Disposed) { throw "$mode leaked the HTTP handler: $errorText" }
    if ($mode -eq 'success') {
        if ($errorText -or [IO.File]::ReadAllBytes($path)[0] -ne 42) { throw "success failed: $errorText" }
        Remove-Item -LiteralPath $path
    } else {
        if (-not $errorText -or (Test-Path -LiteralPath $path)) { throw "$mode did not reject and clean partial output" }
        if ($mode -in @('body', 'headers', 'redirects')) {
            if ($errorText -notmatch 'cancel|canceled' -or $watch.ElapsedMilliseconds -gt 3000) { throw "$mode missed its deadline: $errorText" }
        }
        if ($mode -in @('advertised', 'actual') -and $errorText -notmatch 'size limit') { throw "wrong size failure: $errorText" }
        if ($mode -eq 'limit' -and ($errorText -notmatch 'too many redirects' -or $handler.Requests -ne 11)) { throw "redirect limit failed: $errorText" }
        if ($mode -eq 'downgrade' -and $errorText -notmatch 'HTTPS') { throw "downgrade accepted: $errorText" }
        if ($mode -eq 'missing' -and $errorText -notmatch 'target') { throw "missing redirect accepted: $errorText" }
    }
    if ($mode -ne 'headers' -and -not $handler.Body.Disposed) { throw "$mode leaked the response body" }
    if ($mode -eq 'body' -and (-not $handler.Body.Partial -or -not $handler.Body.ReadToken.IsCancellationRequested)) { throw 'body was not cancelled after partial output' }
    if ($mode -eq 'redirects' -and ($handler.Requests -lt 2 -or $handler.Requests -gt 10 -or $handler.DisposedResponses -lt 1)) { throw 'redirects did not consume the same deadline and dispose responses' }
}
Write-Output 'Validated download deadline and cleanup cases'
`);
      const result = await run("pwsh", ["-NoProfile", "-File", join(directory, "test.ps1")], { timeout: 20000 });
      equal(result.stderr, "", `installer ${index}`);
      match(result.stdout, /Validated download deadline and cleanup cases/);
    }
  } finally { await rm(directory, { recursive: true, force: true }); }
});
