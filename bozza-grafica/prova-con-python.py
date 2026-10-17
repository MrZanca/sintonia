import sys, datetime, ctypes, ctypes.wintypes as wt, slint

u = ctypes.windll.user32
u.FindWindowW.restype = ctypes.c_void_p
u.SetWindowPos.argtypes = [ctypes.c_void_p, ctypes.c_void_p, ctypes.c_int, ctypes.c_int, ctypes.c_int, ctypes.c_int, ctypes.c_uint]
u.GetWindowRect.argtypes = [ctypes.c_void_p, ctypes.c_void_p]
u.GetDpiForWindow.argtypes = [ctypes.c_void_p]
TITLE = 'Sintonia mini player'

def hwnd():
    return u.FindWindowW(None, TITLE) or 0

def rect():
    r = wt.RECT()
    u.GetWindowRect(hwnd(), ctypes.byref(r))
    return r

def put(x, y):
    u.SetWindowPos(ctypes.c_void_p(hwnd()), ctypes.c_void_p(-1), int(x), int(y), 0, 0, 0x0001 | 0x0010)  # topmost, no resize, no activate


mod = slint.load_file(sys.argv[1])
refs = {}


class Pip(mod.PipWindow):
    @slint.callback(name="expand")
    def expand(self):
        self.hide()
        app = refs["app"]
        for i, c in enumerate(app.channels):
            if c.num == self.num:
                app.selected = i
        app.playing = True
        app.show()

    @slint.callback(name="dismiss")
    def dismiss(self):
        self.hide()

    @slint.callback(name="moved")
    def moved(self, dx, dy):
        s = u.GetDpiForWindow(hwnd()) / 96
        r = rect()
        put(r.left + dx * s, r.top + dy * s)


class App(mod.MainWindow):
    @slint.callback(name="request-pip")
    def request_pip(self, ch):
        pip = refs["pip"]
        pip.num, pip.name, pip.now, pip.tint = ch.num, ch.name, ch.now, ch.tint
        pip.show()
        def place():
            s = u.GetDpiForWindow(hwnd()) / 96
            put(u.GetSystemMetrics(0) - 400 * s, u.GetSystemMetrics(1) - 330 * s)
            print('placed', hwnd(), rect().left, rect().top, flush=True)
        slint.Timer.single_shot(datetime.timedelta(milliseconds=700), place)

    @slint.callback(name="close-pip")
    def close_pip(self):
        refs["pip"].hide()


refs["pip"] = Pip()
refs["app"] = App()
refs["app"].run()
