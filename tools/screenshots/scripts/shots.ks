size 120 32
unenv CLAUDE
env COLORTERM=truecolor
env KESTREL_CONFIG_DIR={demo}\profile\config
env KESTREL_STATE_DIR={demo}\profile\state
cwd {demo}\aurora
start {ks} {demo}\aurora
wait 2000
# main: README preview at the root
send G
wait 1200
shot main
# code: src/sky/mod.rs with syntax highlighting
send gg
wait 300
send /
type src 80
send \r
wait 300
send l
wait 500
send /
type sky 80
send \r
send l
wait 600
send /
type mod 80
send \r
wait 1200
shot preview
send \e
wait 200
send h
wait 300
send h
wait 500
# image preview
send /
type assets 80
send \r
send l
wait 500
send /
type aurora 80
send \r
wait 2500
shot image
send \e
send h
wait 600
# find file
send ^P
wait 1500
type sky 150
wait 1500
shot find
send \e
wait 500
# content search
send ^F
wait 600
type sun 150
wait 2000
send ^N
wait 300
send ^N
wait 1200
shot search
send \e
wait 600
# command palette
send :
wait 1200
shot palette
send \e
wait 500
# help
send ?
wait 800
shot help
send \e
wait 500
# drives
send g/
wait 2500
shot drives
quit
