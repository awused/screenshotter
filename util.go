package main

import (
	"errors"
	"regexp"
	"strings"
)

var safeFilenameRegex = regexp.MustCompile(`[^\p{L}\p{N}-_+=]+`)
var repeatedHyphens = regexp.MustCompile(`--+`)
var extraRegex = regexp.MustCompile(`%![(]EXTRA string=.*$`)

func convertApplicationName(input string) string {
	output := extraRegex.ReplaceAllString(input, "")
	output = strings.ToLower(output)
	output = safeFilenameRegex.ReplaceAllString(output, "-")
	output = repeatedHyphens.ReplaceAllString(output, "-")
	return strings.Trim(output, "-")
}

func getVarFromXdotool(output []byte, variable string) (string, error) {
	re := regexp.MustCompile(variable + "=([0-9]+)\n")

	matches := re.FindSubmatch(output)
	if matches == nil {
		return "",
			errors.New("Variable " + variable + " not found in xdotool output")
	}

	return string(matches[1]), nil
}

// Naive linear search, fine for small numbers of items
// User would have to add tens of thousands of items to their configs for this
// to be noticeable
func contains(ss []string, s string) bool {
	for _, st := range ss {
		if st == s {
			return true
		}
	}
	return false
}
