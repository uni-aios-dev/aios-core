# AIOS Shell - Command Reference

AIOS Shell v2.2 - Full-featured command line interface

## Built-in Commands
| Command | Description | Example |
|---------|-------------|---------|
| help, ? | Show all available commands | help |
| exit, quit, q | Exit the shell | exit |
| clear, cls | Clear screen | clear |
| pwd | Print working directory | pwd |
| cd [dir] | Change directory (~, - supported) | cd /etc |
| env | Display all environment variables | env |
| export VAR=value | Set environment variable | export PATH=/bin:$PATH |
| unset VAR | Remove environment variable | unset TEMP |
| history | Show command history | history |
| alias [name=cmd] | List or create aliases | alias ll='ls -la' |
| unalias name | Remove alias | unalias ll |
| source file, . | Execute commands from file | source ~/.aios/shellrc |
| type cmd | Show if command is alias/builtin/external | type ls |
| echo [text...] | Print text | echo $HOME |
| exec cmd [args] | Execute command replacing shell | exec bash |
| jobs | List background jobs | jobs |

## File Operations
| Command | Description | Example |
|---------|-------------|---------|
| ls [opts] | List directory contents | ls -la |
| cat file | Display file contents | cat /etc/hostname |
| head file | Show first lines | head -n 20 file.txt |
| tail file | Show last lines | tail -n 20 file.txt |
| less/more file | View file page by page | less file.txt |
| grep pattern file | Search text patterns | grep root /etc/passwd |
| find path -name pattern | Find files | find / -name *.conf |
| wc file | Count lines/words/bytes | wc -l file.txt |
| cp/mv/rm | Copy/move/remove files | cp a b |
| mkdir/touch | Create dirs/files | mkdir -p dir |
| chmod/chown | Change permissions/ownership | chmod 755 script.sh |
| ln stat file | Links and file info | ln -s target link |

## System Information
| Command | Description | Example |
|---------|-------------|---------|
| ps, top, htop | Process monitoring | ps aux |
| free, df, du | Memory/disk usage | free -h |
| uname, uptime, whoami, id, date | System info | uname -a |
| dmesg | Kernel messages | dmesg \| tail |
| kernel, lsmod | Kernel info/modules | kernel |
| modprobe/insmod/rmmod | Load/unload modules | modprobe loop |
| kill/killall | Terminate processes | kill -9 PID |

## Network
| Command | Description | Example |
|---------|-------------|---------|
| ping, curl, wget | Network testing/downloading | ping 8.8.8.8 |
| ip, ifconfig, route, arp | Network config | ip addr show |
| ss, netstat | Socket statistics | ss -tulpn |
| nc | Netcat connections | nc host port |
| nslookup, dig, traceroute | DNS/tracing | dig google.com |

## Debugging & Hardware
| Command | Description | Example |
|---------|-------------|---------|
| lscpu, lsblk, lsusb, lspci | Hardware info | lspci -nnk |
| lsof, strace | Debugging tools | lsof -i |
| dmidecode, hdparm, iostat, vmstat | Low-level info/stats | vmstat 2 5 |
| mount, umount | Filesystem mounting | mount /dev/sdb1 /mnt |

## Development Tools
| Command | Description | Example |
|---------|-------------|---------|
| git, make, cmake | VCS/build tools | git status |
| gcc,g++,rustc,cargo | Compilers/builders | cargo build |
| python,python3,node,npm,pip | Interpreters/package managers | python3 script.py |

## Archive Tools
| Command | Description | Example |
|---------|-------------|---------|
| tar,gzip,bzip2,xz,zip,unzip | Compression/archival | tar -xzf file.tar.gz |

## AIOS Specific
| Command | Description | Example |
|---------|-------------|---------|
| gui | Launch AIOS GUI | gui |
| tui | Launch AIOS TUI | tui |
| aios | Run AIOS kernel | aios --safe-mode |
| logs | Show system/AIOS logs | logs |
| hw/hardware/info | Hardware details | hw |
| services | Running processes | services |
| aios-shell-reload | Reload shellrc | aios-shell-reload |

## Editors
| Command | Description | Example |
|---------|-------------|---------|
| vi/vim/nano/emacs/micro | Text editors | vim file.txt |
| less/more | Pagers | less README.md |

## Features
- Command chaining: `cmd1; cmd2` or `cmd1 && cmd2`
- Background execution: `command &`
- Aliases and shellrc support
- Command history persistence
- Variable expansion
- Automatic GUI app launching in X session
- Runs external binaries from PATH via shell fallback
