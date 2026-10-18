#!/bin/sh
# Paints colour ramps so encoders have photo-like areas (Tight gradient/JPEG) next to plain text.
for row in 0 1 2 3 4 5; do
  i=0
  while [ $i -lt 240 ]; do printf '\033[48;5;%dm ' $(( (i + row * 16) % 256 )); i=$((i + 1)); done
  printf '\033[0m\n'
done
# Truecolor rows: well over 256 colours, so Tight has to use its copy and gradient filters.
for row in 0 1 2 3 4 5; do
  i=0
  while [ $i -lt 240 ]; do printf '\033[48;2;%d;%d;%dm ' $i $((255 - i)) $(( (i * 3 + row * 40) % 256 )); i=$((i + 1)); done
  printf '\033[0m\n'
done
exec sleep 2147483647
