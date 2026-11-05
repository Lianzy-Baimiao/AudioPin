"""Local AMD protocol probe; reads state only unless --recover is supplied."""
import ctypes as c, struct, winreg, sys, hashlib, time
from pathlib import Path
assert hashlib.sha256(Path(r'C:\Windows\System32\AMD\ANR\AMDNoiseSuppression.exe').read_bytes()).hexdigest() == '4ecec1b76d9ef18c7f2a26da823e71124db627e3acd3c178c89b80b3aeac20ee', 'Unverified AMD binary; refusing IPC'
k=c.WinDLL('kernel32',use_last_error=True)
for name,args,ret in [('CreateMutexW',[c.c_void_p,c.c_bool,c.c_wchar_p],c.c_void_p),('ReleaseMutex',[c.c_void_p],c.c_bool),('OpenFileMappingW',[c.c_uint32,c.c_bool,c.c_wchar_p],c.c_void_p),('MapViewOfFile',[c.c_void_p,c.c_uint32,c.c_uint32,c.c_uint32,c.c_size_t],c.c_void_p),('OpenEventW',[c.c_uint32,c.c_bool,c.c_wchar_p],c.c_void_p),('SetEvent',[c.c_void_p],c.c_bool),('ResetEvent',[c.c_void_p],c.c_bool),('WaitForSingleObject',[c.c_void_p,c.c_uint32],c.c_uint32),('CloseHandle',[c.c_void_p],c.c_bool),('UnmapViewOfFile',[c.c_void_p],c.c_bool)]:
 f=getattr(k,name); f.argtypes=args; f.restype=ret
h=k.OpenFileMappingW(6,False,'Local\\AMDANRMemoryMap'); assert h, c.get_last_error()
p=k.MapViewOfFile(h,6,0,0,0x29e08); assert p
s=k.OpenEventW(0x100002,False,'Local\\ANR_EVT_S'); d=k.OpenEventW(0x100002,False,'Local\\ANR_EVT_D'); assert s and d
mutex=k.CreateMutexW(None,False,'Local\\AudioPin.AmdIpc.v1')
try:
 with winreg.OpenKey(winreg.HKEY_CURRENT_USER,r'Software\AMD\CN\Audio') as reg: device=winreg.QueryValueEx(reg,'ANRAudioInputDevice')[0].split('\0')[0]
 def request(command,offset,result,enabled=None):
  assert k.WaitForSingleObject(mutex,10000) in [0,128]
  try: return request_locked(command,offset,result,enabled)
  finally: k.ReleaseMutex(mutex)
 def request_locked(command,offset,result,enabled=None):
  raw=(device+'\0').encode('utf-16-le'); assert len(raw)<=256
  c.memset(p+offset,0,256); c.memmove(p+offset,raw,len(raw))
  if enabled is not None: c.c_ubyte.from_address(p+offset+256).value=enabled
  c.c_int32.from_address(p+result).value=-999
  c.c_uint32.from_address(p+0x818).value=command
  k.ResetEvent(d); assert k.SetEvent(s)
  wait=k.WaitForSingleObject(d,5000)
  print('command',hex(command),'wait',wait,'rc',c.c_int32.from_address(p+result).value,'state',c.c_ubyte.from_address(p+offset+256).value)
  assert wait==0 and c.c_int32.from_address(p+result).value==0, 'IPC request failed'
  return c.c_ubyte.from_address(p+offset+256).value
 state=request(0x86,0x868,0x96c)
 if '--recover' in sys.argv and not state:
  request(0x87,0x978,0xa7c,1)
  assert request(0x86,0x868,0x96c)==1
 if '--test-watchdog' in sys.argv:
  assert state == 1, 'Expected enabled initial state'
  started=time.monotonic()
  try:
   request(0x87,0x978,0xa7c,0)
   assert request(0x86,0x868,0x96c)==0, 'Fault injection did not disable microphone suppression'
   print('RED: AMD runtime suppression is OFF')
   while time.monotonic()-started < 10:
    time.sleep(0.5)
    if request(0x86,0x868,0x96c)==1:
     print('GREEN: watchdog recovered in %.2f seconds' % (time.monotonic()-started));break
   else: raise AssertionError('Watchdog did not recover within 10 seconds')
  finally:
   request(0x87,0x978,0xa7c,1)
finally:
 k.UnmapViewOfFile(p)
 for x in [h,s,d,mutex]: k.CloseHandle(x)
