p=r"C:/MyProject/AIOS/aios-core/Cargo.toml"
s=open(p).read()
# Remove any broken replacement
s=s.replace('"aios-shell","aios-store"', '"aios-store"')
s=s.replace('aios-store = { path = "aios-shell","aios-store" }', 'aios-store = { path = "aios-store" }')
# Add properly in members
if '"aios-shell"' not in s.split('members')[0] and 'aios-shell' not in s.split('[workspace.dependencies]')[0]:
    # insert before aios-store in members list
    s=s.replace('"aios-store",\n', '"aios-shell",\n    "aios-store",\n')
open(p,'w').write(s)
print(open(p).read().count('aios-shell'))
