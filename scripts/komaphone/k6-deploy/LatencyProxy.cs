// Isolated acceptance fixture only. Credentials and request bodies remain in memory.
// The application/service remain Rust; this helper uses the PowerShell runtime's .NET.
using System;
using System.Collections.Concurrent;
using System.Collections.Generic;
using System.Diagnostics;
using System.IO;
using System.Net;
using System.Net.Http;
using System.Net.Sockets;
using System.Text;
using System.Threading;
using System.Threading.Tasks;

public sealed class K6LatencyProxy : IDisposable
{
    private readonly TcpListener listener;
    private readonly HttpClient http = new HttpClient(new HttpClientHandler { UseProxy = false });
    private readonly CancellationTokenSource stop = new CancellationTokenSource();
    private readonly ConcurrentDictionary<long, Task> handlers = new ConcurrentDictionary<long, Task>();
    private readonly SemaphoreSlim slots = new SemaphoreSlim(64, 64);
    private readonly Task accept;
    private readonly int backend, delay;
    private CancellationTokenSource loadStop;
    private Task[] loadTasks = Array.Empty<Task>();
    private long serial, requests, errors, requestBytes, responseBytes, active, peakActive, loadSent, loadErrors;
    public int Port { get; }

    public K6LatencyProxy(int backendPort, int oneWayDelayMs)
    {
        if (backendPort < 1 || backendPort > 65535 || oneWayDelayMs < 0 || oneWayDelayMs > 1000)
            throw new ArgumentOutOfRangeException(nameof(backendPort));
        backend = backendPort; delay = oneWayDelayMs;
        http.Timeout = TimeSpan.FromSeconds(20);
        listener = new TcpListener(IPAddress.Loopback, 0);
        listener.Start(64);
        Port = ((IPEndPoint)listener.LocalEndpoint).Port;
        accept = Task.Run(Accept);
    }

    private async Task Accept()
    {
        try
        {
            while (!stop.IsCancellationRequested)
            {
                await slots.WaitAsync(stop.Token);
                TcpClient client;
                try { client = await listener.AcceptTcpClientAsync(stop.Token); }
                catch { slots.Release(); throw; }
                long id = Interlocked.Increment(ref serial);
                Task task = Handle(client);
                handlers[id] = task;
                _ = task.ContinueWith(_ => { Task ignored; handlers.TryRemove(id, out ignored); slots.Release(); }, TaskScheduler.Default);
            }
        }
        catch (OperationCanceledException) { }
        catch (ObjectDisposedException) { }
    }

    private async Task Handle(TcpClient client)
    {
        long current = Interlocked.Increment(ref active);
        long observed;
        while ((observed = Interlocked.Read(ref peakActive)) < current && Interlocked.CompareExchange(ref peakActive, current, observed) != observed) { }
        using (client)
        {
            try
            {
                NetworkStream stream = client.GetStream();
                var header = new List<byte>(1024);
                var one = new byte[1];
                while (header.Count < 32768)
                {
                    if (await stream.ReadAsync(one.AsMemory(), stop.Token) != 1) throw new IOException("Incomplete fixture request");
                    header.Add(one[0]);
                    int n = header.Count;
                    if (n >= 4 && header[n - 4] == 13 && header[n - 3] == 10 && header[n - 2] == 13 && header[n - 1] == 10) break;
                }
                if (header.Count >= 32768) throw new IOException("Fixture header limit");
                string[] lines = Encoding.ASCII.GetString(header.ToArray()).Split(new[] { "\r\n" }, StringSplitOptions.None);
                string[] start = lines[0].Split(' ');
                if (start.Length != 3 || !start[1].StartsWith("/", StringComparison.Ordinal) || start[1].StartsWith("//", StringComparison.Ordinal)) throw new IOException("Fixture route");
                int length = 0;
                string authorization = null, contentType = "application/json";
                for (int i = 1; i < lines.Length; i++)
                {
                    int colon = lines[i].IndexOf(':');
                    if (colon < 0) continue;
                    string name = lines[i].Substring(0, colon).Trim(), value = lines[i].Substring(colon + 1).Trim();
                    if (name.Equals("Content-Length", StringComparison.OrdinalIgnoreCase))
                    {
                        if (!int.TryParse(value, out length) || length < 0 || length > 1024 * 1024) throw new IOException("Fixture body limit");
                    }
                    if (name.Equals("Transfer-Encoding", StringComparison.OrdinalIgnoreCase)) throw new IOException("Fixture requires a bounded content length");
                    if (name.Equals("Authorization", StringComparison.OrdinalIgnoreCase)) authorization = value;
                    if (name.Equals("Content-Type", StringComparison.OrdinalIgnoreCase)) contentType = value;
                }
                byte[] body = new byte[length];
                int read = 0;
                while (read < length)
                {
                    int n = await stream.ReadAsync(body.AsMemory(read), stop.Token);
                    if (n == 0) throw new IOException("Incomplete fixture body");
                    read += n;
                }
                Interlocked.Increment(ref requests);
                Interlocked.Add(ref requestBytes, header.Count + length);
                await Task.Delay(delay, stop.Token);
                using var request = new HttpRequestMessage(new HttpMethod(start[0]), "http://127.0.0.1:" + backend + start[1]);
                if (authorization != null) request.Headers.TryAddWithoutValidation("Authorization", authorization);
                if (length != 0)
                {
                    request.Content = new ByteArrayContent(body);
                    request.Content.Headers.TryAddWithoutValidation("Content-Type", contentType);
                }
                using HttpResponseMessage response = await http.SendAsync(request, HttpCompletionOption.ResponseHeadersRead, stop.Token);
                using Stream incoming = await response.Content.ReadAsStreamAsync(stop.Token);
                using var output = new MemoryStream();
                var buffer = new byte[16384];
                int count;
                while ((count = await incoming.ReadAsync(buffer.AsMemory(), stop.Token)) != 0)
                {
                    if (output.Length + count > 2 * 1024 * 1024) throw new IOException("Fixture response limit");
                    output.Write(buffer, 0, count);
                }
                await Task.Delay(delay, stop.Token);
                byte[] bytes = output.ToArray();
                byte[] responseHeader = Encoding.ASCII.GetBytes("HTTP/1.1 " + (int)response.StatusCode + " Fixture\r\nContent-Type: application/json\r\nContent-Length: " + bytes.Length + "\r\nConnection: close\r\n\r\n");
                await stream.WriteAsync(responseHeader.AsMemory(), stop.Token);
                await stream.WriteAsync(bytes.AsMemory(), stop.Token);
                Interlocked.Add(ref responseBytes, responseHeader.Length + bytes.Length);
            }
            catch (OperationCanceledException) { }
            catch { if (!stop.IsCancellationRequested) Interlocked.Increment(ref errors); }
            finally { Interlocked.Decrement(ref active); }
        }
    }

    public void StartLoad(string[] credentials, string[] bodies, int periodMs)
    {
        StopLoad();
        if (credentials.Length != bodies.Length || credentials.Length > 16 || periodMs < 100) throw new ArgumentException("Bounded fixture load");
        loadStop = new CancellationTokenSource();
        CancellationToken cancel = loadStop.Token;
        loadTasks = new Task[credentials.Length];
        Interlocked.Exchange(ref loadSent, 0); Interlocked.Exchange(ref loadErrors, 0);
        for (int i = 0; i < credentials.Length; i++)
        {
            string credential = credentials[i], body = bodies[i];
            loadTasks[i] = Task.Run(async () =>
            {
                try
                {
                    while (!cancel.IsCancellationRequested)
                    {
                        var clock = Stopwatch.StartNew();
                        try
                        {
                            using var request = new HttpRequestMessage(HttpMethod.Post, "http://127.0.0.1:" + Port + "/v1/canvas");
                            request.Headers.TryAddWithoutValidation("Authorization", "Bearer " + credential);
                            request.Content = new StringContent(body, Encoding.UTF8, "application/json");
                            using HttpResponseMessage response = await http.SendAsync(request, cancel);
                            if (!response.IsSuccessStatusCode) Interlocked.Increment(ref loadErrors);
                            Interlocked.Increment(ref loadSent);
                        }
                        catch (OperationCanceledException) when (cancel.IsCancellationRequested) { break; }
                        catch { Interlocked.Increment(ref loadErrors); }
                        int wait = Math.Max(0, periodMs - (int)clock.ElapsedMilliseconds);
                        if (wait > 0) await Task.Delay(wait, cancel);
                    }
                }
                catch (OperationCanceledException) { }
            });
        }
    }

    public void StopLoad()
    {
        if (loadStop == null) return;
        loadStop.Cancel();
        if (!Task.WaitAll(loadTasks, 5000)) throw new IOException("Fixture load did not stop");
        loadStop.Dispose(); loadStop = null; loadTasks = Array.Empty<Task>();
    }
    public object Snapshot() => new
    {
        OneWayDelayMs = delay, Requests = Interlocked.Read(ref requests), Errors = Interlocked.Read(ref errors),
        RequestBytes = Interlocked.Read(ref requestBytes), ResponseBytes = Interlocked.Read(ref responseBytes),
        Active = Interlocked.Read(ref active), PeakActive = Interlocked.Read(ref peakActive),
        LoadSent = Interlocked.Read(ref loadSent), LoadErrors = Interlocked.Read(ref loadErrors), ConnectionLimit = 64
    };
    public void Dispose()
    {
        StopLoad(); stop.Cancel(); listener.Stop();
        try { accept.GetAwaiter().GetResult(); } catch (SocketException) when (stop.IsCancellationRequested) { }
        var pending = new List<Task>(handlers.Values).ToArray();
        if (!Task.WaitAll(pending, 5000)) throw new IOException("Fixture proxy did not stop");
        http.Dispose(); stop.Dispose(); slots.Dispose();
    }
}
