#!/bin/bash
# Demo recording script — proper pacing, highlighted commands
REWIND="$HOME/workspace/rewind/target/release/rewind"

# Bold green for commands, cyan for prompt
P='\033[1;36m'  # cyan prompt
G='\033[1;32m'  # green command
X='\033[0m'     # reset

# Command 1: Show the full trace — the hero output
echo -e "${P}❯${X} ${G}rewind show latest${X}"
sleep 0.3
$REWIND show latest
sleep 4

# Command 2: Diff the main vs fixed timeline
echo ""
echo -e "${P}❯${X} ${G}rewind diff latest main fixed${X}"
sleep 0.3
$REWIND diff latest main fixed
sleep 3.5

# Command 3: List sessions
echo ""
echo -e "${P}❯${X} ${G}rewind sessions${X}"
sleep 0.3
$REWIND sessions
sleep 2.5
