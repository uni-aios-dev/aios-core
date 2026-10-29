import re
p=r"C:/MyProject/AIOS/aios-core/live/build.sh"
s=open(p).read()
m=re.search(r'echo "=== \[1\] building aios \+ aios-gui[\s\S]*?--features base-all',s)
if not m:
    print("no")
    raise SystemExit
old=m.group(0)
neu='echo "=== [1] building aios + aios-gui + aios-shell (musl, webview engine ON, dynamic) ==="\ncd /src\nCARGO_TARGET_DIR=/tmp/target-dyn RUSTFLAGS="-C target-feature=-crt-static" \\\n  cargo build -p aios-gui --release 2>&1 | tail -30\nCARGO_TARGET_DIR=/tmp/target-dyn RUSTFLAGS="-C target-feature=-crt-static" \\\n  cargo build -p aios-shell --release 2>&1 | tail -15\nCARGO_TARGET_DIR=/tmp/target cargo build -p aios --release --no-default-features --features base-all 2>&1 | tail -20'
open(p,"w").write(s.replace(old,neu))
print("ok")
