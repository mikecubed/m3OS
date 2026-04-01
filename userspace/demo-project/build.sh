#!/bin/sh
# build.sh — build and test the demo project
echo "Building demo project..."
/usr/bin/tcc -static -c main.c -o main.o
/usr/bin/tcc -static -c util.c -o util.o
/usr/bin/tcc -static -o demo main.o util.o
echo "Running demo..."
./demo
echo "Build and test complete."
