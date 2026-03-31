CC = tcc
CFLAGS = -static

SRCS = main.c util.c
OBJS = $(SRCS:.c=.o)
TARGET = demo

.PHONY: all clean

all: $(TARGET)

$(TARGET): $(OBJS)
	$(CC) $(CFLAGS) -o $(TARGET) $(OBJS)

.c.o:
	$(CC) $(CFLAGS) -c $< -o $@

clean:
	rm -f $(OBJS) $(TARGET)
