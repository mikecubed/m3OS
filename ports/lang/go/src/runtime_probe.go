// Phase 86d — m3OS Go-runtime probe.
//
// A fully static (CGO_ENABLED=0) Go program that validates the Go runtime
// end-to-end on m3OS without any TLS dependency. It prints three sentinels the
// `go-runtime-smoke` gate waits on:
//
//	GO_HELLO_OK      the Go runtime started — scheduler, GC, and the random
//	                 bootstrap (getrandom/AT_RANDOM) are alive.
//	GO_GOROUTINE_OK  a goroutine pinned to a *second* OS thread (forced via
//	                 runtime.LockOSThread → clone(CLONE_THREAD)) completed a
//	                 channel rendezvous with the main goroutine.
//	GO_HTTP_OK       a plaintext HTTP GET completed over the in-kernel TCP
//	                 stack (sys_connect → tcp::connect), status 200.
//
// Usage: runtime_probe [http://host:port/path]
// With no URL argument the HTTP phase is skipped (still prints GO_HTTP_SKIP).
package main

import (
	"fmt"
	"io"
	"net/http"
	"os"
	"runtime"
	"sync"
	"time"
)

func main() {
	fmt.Println("GO_HELLO_OK")

	// os.Executable resolves via /proc/self/exe (m3OS procfs).
	if exe, err := os.Executable(); err == nil {
		fmt.Printf("GO_EXE=%s\n", exe)
	} else {
		fmt.Printf("GO_EXE_ERR=%v\n", err)
	}
	// GOMAXPROCS derives from sched_getaffinity; report it alongside NumCPU.
	fmt.Printf("GO_GOMAXPROCS=%d GO_NUMCPU=%d\n", runtime.GOMAXPROCS(0), runtime.NumCPU())

	// --- goroutine rendezvous on a second OS thread ---------------------
	// LockOSThread in the child goroutine so the runtime must back it with a
	// distinct M (OS thread, created via clone(CLONE_THREAD)) regardless of
	// GOMAXPROCS. The unbuffered channel forces a real cross-thread rendezvous
	// (futex wake of the main goroutine). GOMAXPROCS is left at the value Go
	// derives from sched_getaffinity (no artificial bump) to keep scheduler
	// concurrency minimal.
	ch := make(chan int)
	var wg sync.WaitGroup
	wg.Add(1)
	go func() {
		defer wg.Done()
		runtime.LockOSThread()
		defer runtime.UnlockOSThread()
		ch <- 0x42
	}()
	select {
	case v := <-ch:
		if v == 0x42 {
			fmt.Println("GO_GOROUTINE_OK")
		} else {
			fmt.Printf("GO_GOROUTINE_BAD=%#x\n", v)
		}
	case <-time.After(10 * time.Second):
		fmt.Println("GO_GOROUTINE_TIMEOUT")
	}
	wg.Wait()

	// --- plaintext HTTP GET over the in-kernel TCP stack ----------------
	url := ""
	if len(os.Args) > 1 {
		url = os.Args[1]
	}
	if url == "" {
		fmt.Println("GO_HTTP_SKIP (no url argument)")
		return
	}
	client := &http.Client{Timeout: 20 * time.Second}
	resp, err := client.Get(url)
	if err != nil {
		fmt.Printf("GO_HTTP_ERR=%v\n", err)
		return
	}
	defer resp.Body.Close()
	body, err := io.ReadAll(resp.Body)
	if err != nil {
		fmt.Printf("GO_HTTP_READ_ERR=%v\n", err)
		return
	}
	fmt.Printf("GO_HTTP_STATUS=%d GO_HTTP_LEN=%d\n", resp.StatusCode, len(body))
	if resp.StatusCode == 200 {
		fmt.Println("GO_HTTP_OK")
	} else {
		fmt.Printf("GO_HTTP_BADSTATUS=%d\n", resp.StatusCode)
	}
}
