package main

import (
	"fmt"
	"log"
	"os"
	"os/exec"
	"strings"
)

// TODO -- urface/cli
func main() {
	if len(os.Args) < 2 {
		log.Fatal("Specify mode in [window, section]")
	}
	fmt.Println(os.Args)

	fmt.Println("vim-go")
}

func runBash(cmd string) (string, error) {
	// See http://redsymbol.net/articles/unofficial-bash-strict-mode/
	command := `
		set -euo pipefail
		IFS=$'\n\t'
		` + cmd + "\n"

	bash := exec.Command("/usr/bin/env", "bash")
	bash.Stdin = strings.NewReader(command)
	bash.Stderr = os.Stderr

	bashOut, err := bash.Output()
	return string(bashOut), err
}
