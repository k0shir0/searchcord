"""Small terminal header and operating-system browser launch helpers."""
import atexit
from contextlib import asynccontextmanager
from multiprocessing import parent_process
import os
import shutil
import sys
import webbrowser


LARGE = [
    ' #####   #######   #####   ######    #####   ##   ##   #####    #####   ######   ###### ',
    '##   ##  ##       ##   ##  ##   ##  ##   ##  ##   ##  ##   ##  ##   ##  ##   ##  ##   ##',
    '##       ##       ##   ##  ##   ##  ##       ##   ##  ##       ##   ##  ##   ##  ##   ##',
    ' #####   ######   #######  ######   ##       #######  ##       ##   ##  ######   ##   ##',
    '     ##  ##       ##   ##  ## ##    ##       ##   ##  ##       ##   ##  ## ##    ##   ##',
    '##   ##  ##       ##   ##  ##  ##   ##   ##  ##   ##  ##   ##  ##   ##  ##  ##   ##   ##',
    ' #####   #######  ##   ##  ##   ##   #####   ##   ##   #####    #####   ##   ##  ###### ',
]
WIDE = [
    ' #### #####  ###  ####   #### #   #  ####  ###  ####  #### ',
    '#     #     #   # #   # #     #   # #     #   # #   # #   #',
    '#     #     #   # #   # #     #   # #     #   # #   # #   #',
    ' ###  ####  ##### ####  #     ##### #     #   # ####  #   #',
    '    # #     #   # # #   #     #   # #     #   # # #   #   #',
    '    # #     #   # #  #  #     #   # #     #   # #  #  #   #',
    '####  ##### #   # #   #  #### #   #  ####  ###  #   # #### ',
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

_terminal_started = False
_terminal_stream = None


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


def print_banner(*, width=None, stream=None, max_height=None):
    stream = stream or sys.stdout
    # Leave the last column unused to avoid console auto-wrap at the margin.
    width = max(1, (width if width is not None else shutil.get_terminal_size().columns) - 1)
    color = ansi_supported(stream)
    padding = 2 if color else 0
    available = max_height - padding if max_height is not None else None
    if color and width >= max(map(len, LARGE)) and (available is None or available >= len(LARGE)):
        lines = LARGE
    elif width >= max(map(len, WIDE)) and (available is None or available >= len(WIDE)):
        lines = WIDE
    elif width >= max(map(len, STACKED)) and (available is None or available >= len(STACKED)):
        lines = STACKED
    elif width >= 14:
        lines = ["[ searchcord ]"]
    else:
        lines = ["searchcord"[i:i+width] for i in range(0, 10, width)]
    if available is not None and len(lines) > max(1, available):
        lines = ["searchcord"[:width]]
    if color:
        try:
            "\u2588".encode(getattr(stream, "encoding", None) or "utf-8")
        except (UnicodeEncodeError, LookupError):
            block = "#"
        else:
            block = "\u2588"
        if max_height is None or max_height >= len(lines) + 2:
            lines = ["", *lines, ""]
        # Fill the last column with background color without triggering wrap.
        lines = ["\033[48;2;255;255;255m\033[38;2;141;216;176m"
                 + line.replace("#", block).center(width) + "\033[K\033[0m"
                 for line in lines]
    print("\n".join(lines) + "\n", file=stream, flush=True)
    return len(lines) + 1


def start_terminal():
    """Draw one startup header and reserve the rows below it for log output."""
    global _terminal_started, _terminal_stream
    if _terminal_started:
        return
    _terminal_started = True
    # Uvicorn workers share their parent's console; none can own its viewport.
    if parent_process() is not None:
        return
    stream = sys.stdout
    if not ansi_supported(stream):
        print_banner(stream=stream)
        return
    size = shutil.get_terminal_size()
    stream.write("\033[0m\033[r\033[H\033[2J")
    rows = print_banner(width=size.columns, stream=stream,
                        max_height=max(1, size.lines - 5))
    log_start = rows + 1
    if log_start < size.lines:
        # An omitted bottom margin follows the viewport's current height.
        stream.write(f"\033[{log_start};r\033[{log_start};1H")
        stream.flush()
        _terminal_stream = stream
        atexit.register(stop_terminal)


def stop_terminal():
    """Restore full-screen scrolling without moving the shell's next prompt."""
    global _terminal_stream
    stream, _terminal_stream = _terminal_stream, None
    if stream is not None and not stream.closed:
        stream.write("\0337\033[r\0338\033[0m")
        stream.flush()


@asynccontextmanager
async def terminal_lifespan(_app):
    """Restore the terminal before the server re-raises its shutdown signal."""
    start_terminal()
    try:
        yield
    finally:
        stop_terminal()


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
