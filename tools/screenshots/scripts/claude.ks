size 120 34
unenv CLAUDE
env COLORTERM=truecolor
env KESTREL_CONFIG_DIR={demo}\profile\config
env KESTREL_STATE_DIR={demo}\profile\state
cwd {demo}\aurora
start {ks} {demo}\aurora
wait 2000
send c
wait 6000
send \e[B
wait 500
send \r
wait 5000
send \e=
wait 300
send \e=
wait 300
send \e=
wait 800
send ^\
wait 500
send /
type src 60
send \r
send l
wait 400
send /
type sky 60
send \r
send l
wait 600
send \s
wait 200
send \s
wait 300
send C
wait 1500
type how does the sun move across the sky? 40
wait 1500
shot claude
quit
