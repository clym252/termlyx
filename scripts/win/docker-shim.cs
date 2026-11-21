// VM side of the `fixture` step's docker relay: a `docker.exe` that forwards
// its arguments to scripts/win/docker-relay.py on the Mac (through the step's
// reverse SSH tunnel) and replays the exit code, stdout and stderr. See
// scripts/win/README.md. steps.ps1 compiles it with Add-Type (C# 5, .NET
// Framework), so keep to language features that compiler has.
//
// Relay address: TETHER_DOCKER_RELAY (host:port), default 127.0.0.1:22375.
// Wire format: see docker-relay.py.
using System;
using System.IO;
using System.Net.Sockets;
using System.Text;

public static class DockerShim
{
    public static int Main(string[] args)
    {
        string addr = Environment.GetEnvironmentVariable("TETHER_DOCKER_RELAY");
        if (String.IsNullOrEmpty(addr)) { addr = "127.0.0.1:22375"; }
        int colon = addr.LastIndexOf(':');
        try
        {
            using (TcpClient client = new TcpClient(addr.Substring(0, colon), Int32.Parse(addr.Substring(colon + 1))))
            using (NetworkStream net = client.GetStream())
            {
                WriteU32(net, (uint)args.Length);
                foreach (string a in args)
                {
                    byte[] b = new UTF8Encoding(false).GetBytes(a);
                    WriteU32(net, (uint)b.Length);
                    net.Write(b, 0, b.Length);
                }
                int code = (int)ReadU32(net);
                byte[] stdout = ReadBlock(net);
                byte[] stderr = ReadBlock(net);
                using (Stream o = Console.OpenStandardOutput()) { o.Write(stdout, 0, stdout.Length); }
                using (Stream e = Console.OpenStandardError()) { e.Write(stderr, 0, stderr.Length); }
                return code;
            }
        }
        catch (Exception ex)
        {
            Console.Error.WriteLine("docker shim: relay at {0} failed: {1}", addr, ex.Message);
            return 125;
        }
    }

    static void WriteU32(Stream s, uint v)
    {
        byte[] b = { (byte)(v >> 24), (byte)(v >> 16), (byte)(v >> 8), (byte)v };
        s.Write(b, 0, 4);
    }

    static uint ReadU32(Stream s)
    {
        byte[] b = ReadExact(s, 4);
        return ((uint)b[0] << 24) | ((uint)b[1] << 16) | ((uint)b[2] << 8) | b[3];
    }

    static byte[] ReadBlock(Stream s)
    {
        return ReadExact(s, (int)ReadU32(s));
    }

    static byte[] ReadExact(Stream s, int n)
    {
        byte[] buf = new byte[n];
        int got = 0;
        while (got < n)
        {
            int r = s.Read(buf, got, n - got);
            if (r <= 0) { throw new EndOfStreamException("relay closed the connection"); }
            got += r;
        }
        return buf;
    }
}
