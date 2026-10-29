import re
p=r"C:/MyProject/AIOS/aios-core/live/build.sh"
s=open(p).read()
m=re.search(r'cp /tmp/target-dyn/release/aios-gui "\$W/aios-gui-bin"[\s\S]*?echo "\[build\.sh\] copied binaries:".*?aios-gui.*?\n',s)
if not m:
    print("no")
    raise SystemExit
old=m.group(0)
neu='cp /tmp/target-dyn/release/aios-gui "$W/aios-gui-bin"\ncp /tmp/target-dyn/release/aios-shell "$W/aios-shell-bin" 2>/dev/null || true\nls -la "$W/aios-bin" "$W/aios-gui-bin" "$W/aios-shell-bin" "$W/aios-static-bin"\nfile "$W/aios-gui-bin" 2>/dev/null || true\n'
open(p,"w").write(s.replace(old,neu))
print("ok")
