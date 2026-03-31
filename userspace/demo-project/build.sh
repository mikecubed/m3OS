#!/bin/sh
# build.sh — build and test the demo project
echo "Building demo project..."
tcc -static -c main.c -o main.o
tcc -static -c util.c -o util.o
tcc -static -o demo main.o util.o
echo "Running demo..."
./demo
echo "Build and test complete."
