size 116 30
unenv CLAUDE
env COLORTERM=truecolor
env KESTREL_CONFIG_DIR={demo}\profile\config
env KESTREL_STATE_DIR={demo}\profile\state
cwd {demo}\aurora
start {ks} {demo}\aurora
wait 2500
rec tour
wait 1000
send j
wait 280
send j
wait 280
send j
wait 280
send j
wait 280
send j
wait 280
send j
wait 900
send l
wait 700
send j
wait 300
send j
wait 300
send j
wait 300
send j
wait 1800
send h
wait 900
send ^P
wait 900
type scat 220
wait 1400
send \r
wait 1800
send ^F
wait 800
type sun 220
wait 1500
send ^N
wait 450
send ^N
wait 450
send ^N
wait 1600
send \e
wait 800
send :
wait 1400
type tab 220
wait 1600
send \e
wait 900
send g/
wait 2400
endrec
quit
