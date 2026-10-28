// Deterministic HTTP transport for the production downloader; no sockets or TLS overrides.
using System;
using System.IO;
using System.Net;
using System.Net.Http;
using System.Threading;
using System.Threading.Tasks;

public sealed class DownloadFixture : HttpClientHandler {
    public static string Mode;
    public static volatile bool HandlerDisposed, StreamDisposed, ContentDisposed, Cancelled, Released;
    public static int Reads, Requests;
    public static CancellationToken Token;
    public static void Reset(string mode) {
        Mode = mode;
        HandlerDisposed = StreamDisposed = ContentDisposed = Cancelled = Released = false;
        Reads = Requests = 0;
    }
    readonly TaskCompletionSource<HttpResponseMessage> headers = new TaskCompletionSource<HttpResponseMessage>();
    HttpResponseMessage Response(HttpRequestMessage request) {
        var response = new HttpResponseMessage(HttpStatusCode.OK);
        response.RequestMessage = request;
        response.Content = new FixtureContent();
        if (Mode == "declared") response.Content.Headers.ContentLength = 11;
        if (Mode == "http-error") response.StatusCode = HttpStatusCode.Forbidden;
        if (Mode == "redirect" && Requests == 1 || Mode == "redirect-budget" && Requests <= 3 || Mode == "redirect-stall" && Requests == 1 || Mode == "redirect-limit" || Mode == "downgrade" || Mode == "missing-target") {
            response.StatusCode = HttpStatusCode.Found;
            if (Mode != "missing-target") response.Headers.Location = new Uri(Mode == "downgrade" ? "http://fixture.invalid/file" : "/next", UriKind.RelativeOrAbsolute);
        }
        return response;
    }
    protected override async Task<HttpResponseMessage> SendAsync(HttpRequestMessage request, CancellationToken token) {
        Token = token;
        Interlocked.Increment(ref Requests);
        if (Mode == "headers") return await headers.Task.ConfigureAwait(false); // deliberately ignores cancellation
        if (Mode == "redirect-budget" || Mode == "redirect-stall" && Requests == 1) await Task.Delay(300, token).ConfigureAwait(false);
        return Response(request);
    }
    protected override void Dispose(bool disposing) {
        if (HandlerDisposed) return;
        HandlerDisposed = true;
        base.Dispose(disposing);
        if (Mode == "headers") Task.Run(async () => {
            await Task.Delay(50).ConfigureAwait(false);
            headers.TrySetResult(Response(new HttpRequestMessage(HttpMethod.Get, "https://fixture.invalid/file")));
            Released = true;
        });
    }
    sealed class FixtureContent : HttpContent {
        readonly FixtureStream stream = new FixtureStream();
        readonly TaskCompletionSource<Stream> acquisition = new TaskCompletionSource<Stream>();
        protected override Task<Stream> CreateContentReadStreamAsync() {
            return Mode == "acquisition" ? acquisition.Task : Task.FromResult<Stream>(stream);
        }
        protected override Task SerializeToStreamAsync(Stream target, TransportContext context) { throw new NotSupportedException(); }
        protected override bool TryComputeLength(out long length) { length = 0; return false; }
        protected override void Dispose(bool disposing) {
            ContentDisposed = true;
            // During acquisition the stream is not owned by HttpContent yet. Return it late
            // so the downloader, rather than this fixture, must release it.
            if (Mode == "acquisition") Task.Run(async () => {
                await Task.Delay(50).ConfigureAwait(false);
                acquisition.TrySetResult(stream);
                Released = true;
            });
            else stream.Dispose();
            base.Dispose(disposing);
            if (Mode == "cleanup-error") throw new IOException("fixture disposal failure");
        }
    }
    sealed class FixtureStream : Stream {
        readonly TaskCompletionSource<int> pending = new TaskCompletionSource<int>();
        public override async Task<int> ReadAsync(byte[] buffer, int offset, int count, CancellationToken token) {
            int read = Interlocked.Increment(ref Reads);
            if (Mode == "success" || Mode == "redirect" || Mode == "overflow") {
                if (read > (Mode == "overflow" ? 2 : 1)) return 0;
                int length = Mode == "overflow" ? (read == 1 ? 6 : 5) : 10;
                for (int i = 0; i < length; i++) buffer[offset + i] = (byte)(97 + i);
                return length;
            }
            if (Mode == "progress") { await Task.Delay(300, token).ConfigureAwait(false); buffer[offset] = 97; return 1; }
            if (read == 1) { buffer[offset] = 97; return 1; }
            token.Register(() => { Cancelled = true; if (Mode != "ignore") pending.TrySetCanceled(); });
            return await pending.Task.ConfigureAwait(false);
        }
        public override int Read(byte[] buffer, int offset, int count) { return ReadAsync(buffer, offset, count, CancellationToken.None).GetAwaiter().GetResult(); }
        protected override void Dispose(bool disposing) { StreamDisposed = true; pending.TrySetCanceled(); Released = true; base.Dispose(disposing); }
        public override bool CanRead { get { return true; } }
        public override bool CanSeek { get { return false; } }
        public override bool CanWrite { get { return false; } }
        public override long Length { get { throw new NotSupportedException(); } }
        public override long Position { get { throw new NotSupportedException(); } set { throw new NotSupportedException(); } }
        public override void Flush() { }
        public override long Seek(long offset, SeekOrigin origin) { throw new NotSupportedException(); }
        public override void SetLength(long value) { throw new NotSupportedException(); }
        public override void Write(byte[] buffer, int offset, int count) { throw new NotSupportedException(); }
    }
}
