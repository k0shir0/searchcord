"""Small terminal header and operating-system browser launch helpers."""
import os
import shutil
import sys
import webbrowser


WIDE = [
    ' #### #####  ###  ####   #### ## ##  ####  ###  ####  ####',
    '##    ##    ## ## ## ## ##    ## ## ##    ## ## ## ## ## ##',
    ' ###  ####  ##### ####  ##    ##### ##    ## ## ####  ## ##',
    '   ## ##    ## ## ## #  ##    ## ## ##    ## ## ## #  ## ##',
    '####  ##### ## ## ##  #  #### ## ##  ####  ###  ##  # ####',
]
STACKED = [
    ' #### #####  ###  ####   #### ## ##',
    '##    ##    ## ## ## ## ##    ## ##',
    ' ###  ####  ##### ####  ##    #####',
    '   ## ##    ## ## ## #  ##    ## ##',
    '####  ##### ## ## ##  #  #### ## ##',
    '',
    ' ####  ###  ####  ####',
    '##    ## ## ## ## ## ##',
    '##    ## ## ####  ## ##',
    '##    ## ## ## #  ## ##',
    ' ####  ###  ##  # ####',
]


def ansi_supported(stream):
    if "NO_COLOR" in os.environ or os.environ.get("TERM") == "dumb" or not stream.isatty():
        return False
    if os.name != "nt":
        return True
    # Enable virtual-terminal processing in native Windows consoles. Redirected
    # output and older consoles get plain ASCII.
    try:
        import ctypes
        import msvcrt
        handle = msvcrt.get_osfhandle(stream.fileno())
        mode = ctypes.c_ulong()
        kernel = ctypes.windll.kernel32
        return bool(kernel.GetConsoleMode(ctypes.c_void_p(handle), ctypes.byref(mode))
                    and kernel.SetConsoleMode(ctypes.c_void_p(handle), mode.value | 4))
    except (OSError, ValueError, AttributeError):
        return False


def print_banner(*, width=None, stream=None):
    stream = stream or sys.stdout
    # Leave the last column unused to avoid console auto-wrap at the margin.
    width = max(1, (width if width is not None else shutil.get_terminal_size().columns) - 1)
    if width >= max(map(len, WIDE)):
        lines = WIDE
    elif width >= max(map(len, STACKED)):
        lines = STACKED
    elif width >= 14:
        lines = ["[ searchcord ]"]
    else:
        lines = ["searchcord"[i:i+width] for i in range(0, 10, width)]
    color = ansi_supported(stream)
    print("\n".join(("\033[92m" + line + "\033[0m") if color else line
                    for line in lines) + "\n", file=stream, flush=True)


def local_url(host, port):
    host = {"0.0.0.0": "127.0.0.1", "::": "::1"}.get(host, host)
    return f"http://{'[' + host + ']' if ':' in host else host}:{port}"


def open_url(url):
    print(f"Searchcord: {url}", flush=True)
    try:
        opened = webbrowser.open(url)
    except (OSError, webbrowser.Error):
        opened = False
    if not opened:
        print(f"Could not open the default browser. Open {url} manually.", flush=True)
    return opened
