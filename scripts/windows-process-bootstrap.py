"""Launch a local-gate target only after its bootstrap belongs to the owned job."""
import msvcrt
import os
import struct
import subprocess
import sys

TOKEN = b"SONICTERM-JOB-START-v1\n"


def main():
    """Keep the target's DWORD exit separate from bootstrap protocol failures."""
    startup = msvcrt.open_osfhandle(int(sys.argv[1]), os.O_RDONLY | os.O_BINARY)
    status = msvcrt.open_osfhandle(int(sys.argv[2]), os.O_WRONLY | os.O_BINARY)
    os.set_inheritable(startup, False)
    os.set_inheritable(status, False)
    try:
        data = bytearray()
        while len(data) <= len(TOKEN):
            chunk = os.read(startup, len(TOKEN) + 1 - len(data))
            if not chunk:
                break
            data.extend(chunk)
        os.close(startup)
        startup = None
        if data != TOKEN:
            return 91
        try:
            process = subprocess.Popen(sys.argv[3:], stdin=subprocess.DEVNULL,
                                       stdout=sys.stdout.buffer, stderr=sys.stdout.buffer, close_fds=True)
        except OSError as error:
            record = b"FAIL" + struct.pack("<I", error.winerror or error.errno or 1)
        else:
            record = b"EXIT" + struct.pack("<I", process.wait() & 0xFFFFFFFF)
        os.write(status, record)
        return 0
    finally:
        if startup is not None:
            os.close(startup)
        os.close(status)


if __name__ == "__main__":
    raise SystemExit(main())
