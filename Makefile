# vdisplay: single entrypoint for build, lint, test, security and release.
# Targets wrap the cargo commands from README.md and the release workflow in
# .github/workflows/release.yml; run `make` for the list.

.DEFAULT_GOAL := help
SHELL := /bin/sh

VERSION   ?= $(shell sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
SEVERITY  ?= HIGH,CRITICAL
ARGS      ?=
TARGETS   := aarch64-apple-darwin x86_64-apple-darwin
DIST      := target/dist
SEC_OUT   := target/security
APP       := target/release/vdisplay.app
APP_DEST  ?= $(HOME)/Applications
ICONSET   := target/icon/AppIcon.iconset
NOTICES   := target/THIRD-PARTY-LICENSES.txt
# macOS ties Accessibility / Screen Recording grants to the signature. Ad hoc
# ("-") is a new identity on every build, so the grants stop matching; a
# certificate keeps them. Default: the one `make signing-cert` creates, if it
# is in the keychain, else ad hoc. Override with a Developer ID if you have one.
SIGN_CERT := vdisplay Local Signing
ifeq ($(origin SIGN_IDENTITY),undefined)
SIGN_IDENTITY := $(shell security find-identity -p codesigning 2>/dev/null | grep -q '"$(SIGN_CERT)"' && echo '$(SIGN_CERT)' || echo -)
endif

# Cross-compiling needs a rustup toolchain. A Homebrew `cargo` earlier in PATH
# ignores rustup targets, so dist puts the active rustup toolchain first.
TOOLCHAIN_BIN := $(shell dirname "$$(rustup which rustc 2>/dev/null)" 2>/dev/null)
DIST_PATH     := $(if $(TOOLCHAIN_BIN),$(TOOLCHAIN_BIN):)$(PATH)

# Scanners run as throwaway containers, pinned so local runs match each other.
# The named volume caches Trivy's vulnerability DB between runs.
TRIVY_IMAGE    ?= aquasec/trivy:0.76.0
GITLEAKS_IMAGE ?= zricethezav/gitleaks:v8.30.1
ACTIONLINT_IMAGE ?= rhysd/actionlint:1.7.12
TRIVY          := docker run --rm -v vdisplay-trivy-cache:/root/.cache/trivy
# Trivy resolves .trivyignore against its own CWD inside the container, so the
# file has to be mounted and pointed at explicitly for the allowlist to apply.
TRIVY_IGNORE_VOL := -v "$(CURDIR)/.trivyignore":/.trivyignore
TRIVY_IGNORE_ARG := --ignorefile /.trivyignore
# gitleaks runs as root over a bind mount owned by another uid; without this
# git aborts with "detected dubious ownership in repository".
GITLEAKS_GIT := -e GIT_CONFIG_COUNT=1 -e GIT_CONFIG_KEY_0=safe.directory -e GIT_CONFIG_VALUE_0=/repo

# Personal data that should never be published: absolute home-directory paths
# and e-mail addresses other than GitHub noreply / example domains.
PRIVATE_PATHS  := /(Users|home)/[A-Za-z0-9._-]+/
PRIVATE_EMAILS := [A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}
# git@github.com is the SSH clone address, not a mailbox.
PRIVATE_ALLOW  := noreply|@example\.(com|org)|:git@github\.com$$

##@ 📖 Help

help: ## Show this help
	@awk 'BEGIN {FS = ":.*##"} \
		/^##@/ { printf "\n\033[1m%s\033[0m\n", substr($$0, 5); next } \
		/^[a-zA-Z_0-9-]+:.*?##/ { printf "  \033[36m%-14s\033[0m %s\n", $$1, $$2 }' \
		$(MAKEFILE_LIST)
	@printf "\n\033[2mVars: VERSION SEVERITY ARGS SIGN_IDENTITY APP_DEST TRIVY_IMAGE GITLEAKS_IMAGE\033[0m\n\n"

##@ 📦 Setup

deps: ## Download crates from Cargo.lock
	cargo fetch --locked

tools: ## Install cargo-audit and the rustup targets used by dist
	cargo install cargo-audit --locked
	rustup target add $(TARGETS)

##@ 🏗️ Build

build: ## Release build for this Mac (target/release/vdisplay)
	cargo build --release --locked

# $(call bundle,<vdisplay binary>,<.app dir>): assemble and sign vdisplay.app.
define bundle
	rm -rf $(2)
	mkdir -p $(2)/Contents/MacOS $(2)/Contents/Resources
	cp $(1) $(2)/Contents/MacOS/vdisplay
	sed 's/@VERSION@/$(VERSION)/g' packaging/Info.plist > $(2)/Contents/Info.plist
	cp packaging/AppIcon.icns $(2)/Contents/Resources/AppIcon.icns
	cp LICENSE $(NOTICES) $(2)/Contents/Resources/
	codesign --force --sign "$(SIGN_IDENTITY)" $(2)
endef

notices: ## Third-party license texts of everything in the binary (target/THIRD-PARTY-LICENSES.txt)
	@mkdir -p target
	python3 packaging/third-party-licenses.py $(NOTICES)

dist: notices ## Universal tarball (binary + vdisplay.app, for the formula) + app zip (cask) + sha256s in target/dist
	@for t in $(TARGETS); do \
		PATH="$(DIST_PATH)" cargo build --release --locked --target $$t || exit 1; \
	done
	@rm -rf $(DIST) && mkdir -p $(DIST)/vdisplay-$(VERSION)
	lipo -create $(foreach t,$(TARGETS),target/$(t)/release/vdisplay) \
		-output $(DIST)/vdisplay-$(VERSION)/vdisplay
	$(call bundle,$(DIST)/vdisplay-$(VERSION)/vdisplay,$(DIST)/vdisplay-$(VERSION)/vdisplay.app)
	cp README.md LICENSE $(NOTICES) $(DIST)/vdisplay-$(VERSION)/
	COPYFILE_DISABLE=1 tar -C $(DIST) -czf $(DIST)/vdisplay-$(VERSION)-macos-universal.tar.gz vdisplay-$(VERSION)
	cd $(DIST) && shasum -a 256 vdisplay-$(VERSION)-macos-universal.tar.gz \
		> vdisplay-$(VERSION)-macos-universal.tar.gz.sha256
	cd $(DIST)/vdisplay-$(VERSION) && ditto -c -k --keepParent vdisplay.app ../vdisplay-$(VERSION)-macos-app.zip
	cd $(DIST) && shasum -a 256 vdisplay-$(VERSION)-macos-app.zip \
		> vdisplay-$(VERSION)-macos-app.zip.sha256
	@lipo -info $(DIST)/vdisplay-$(VERSION)/vdisplay
	@ls -1 $(DIST)/*.tar.gz* $(DIST)/*.zip*

install: ## cargo install into ~/.cargo/bin
	cargo install --path . --locked

app: build notices ## Menu bar app bundle for this Mac (target/release/vdisplay.app)
	$(call bundle,target/release/vdisplay,$(APP))
	@echo "built $(APP) (signed: $(SIGN_IDENTITY))"

icon: ## Redraw packaging/AppIcon.icns and docs/icon.png from packaging/make-icon.swift (needs Xcode CLT)
	@rm -rf $(ICONSET) && mkdir -p $(ICONSET)
	swift packaging/make-icon.swift target/icon/icon-1024.png
	@for s in 16 32 128 256 512; do \
		sips -z $$s $$s target/icon/icon-1024.png --out $(ICONSET)/icon_$${s}x$${s}.png >/dev/null || exit 1; \
		d=$$((s * 2)); \
		sips -z $$d $$d target/icon/icon-1024.png --out $(ICONSET)/icon_$${s}x$${s}@2x.png >/dev/null || exit 1; \
	done
	iconutil -c icns $(ICONSET) -o packaging/AppIcon.icns
	sips -z 256 256 target/icon/icon-1024.png --out docs/icon.png >/dev/null
	@echo "wrote packaging/AppIcon.icns and docs/icon.png (README)"

signing-cert: ## Create a self-signed signing certificate (login keychain) so permissions survive rebuilds
	@if security find-identity -p codesigning 2>/dev/null | grep -q '"$(SIGN_CERT)"'; then \
		echo "✅ \"$(SIGN_CERT)\" is already in your keychain"; exit 0; fi; \
	echo "About to add a self-signed code-signing certificate \"$(SIGN_CERT)\" to your login keychain."; \
	printf "Proceed? [y/N] "; read ans; [ "$$ans" = "y" ] || { echo "aborted"; exit 1; }; \
	tmp=$$(mktemp -d) && trap 'rm -rf "$$tmp"' EXIT && \
	/usr/bin/openssl req -x509 -newkey rsa:2048 -nodes -days 3650 -subj "/CN=$(SIGN_CERT)" \
		-addext "keyUsage=critical,digitalSignature" -addext "extendedKeyUsage=critical,codeSigning" \
		-keyout "$$tmp/key.pem" -out "$$tmp/cert.pem" 2>/dev/null && \
	/usr/bin/openssl pkcs12 -export -inkey "$$tmp/key.pem" -in "$$tmp/cert.pem" -name "$(SIGN_CERT)" \
		-passout pass:vdisplay -out "$$tmp/id.p12" && \
	security import "$$tmp/id.p12" -k "$(HOME)/Library/Keychains/login.keychain-db" -P vdisplay -T /usr/bin/codesign && \
	echo "✅ added \"$(SIGN_CERT)\". make app / install-app now sign with it (macOS may ask once to let codesign use the key: Always Allow)."

install-app: app ## Copy vdisplay.app to ~/Applications (quit a running one first)
	@mkdir -p $(APP_DEST)
	rm -rf $(APP_DEST)/vdisplay.app
	cp -R $(APP) $(APP_DEST)/
	@echo "installed $(APP_DEST)/vdisplay.app — open it, or: $(APP_DEST)/vdisplay.app/Contents/MacOS/vdisplay service install"

##@ 🧪 Test

test: ## Run unit tests (resolution parsing, size suggestions)
	cargo test --locked

verify: format-check lint test build ## Format, clippy, tests and release build (what CI runs)
	@echo "✅ verify passed"

##@ 🎨 Lint & format

lint: ## Clippy on all targets, warnings are errors
	cargo clippy --all-targets --locked -- -D warnings

fmt: ## Format the code
	cargo fmt

format-check: ## Check formatting without writing
	cargo fmt --check

lint-workflows: require-docker ## actionlint + shellcheck on .github/workflows
	docker run --rm -v "$(CURDIR)":/repo -w /repo $(ACTIONLINT_IMAGE) -no-color

##@ 🚀 Run

# Without ARGS clap prints help and exits 2, which make reports as a failure.
run: ## Run the CLI with ARGS, e.g. make run ARGS="suggest" (no ARGS: --help)
	cargo run --release --locked -- $(if $(strip $(ARGS)),$(ARGS),--help)

##@ 🔒 Security

audit: ## Dependency CVEs from RustSec (needs cargo-audit: make tools)
	@command -v cargo-audit >/dev/null 2>&1 || { \
		echo "❌ cargo-audit not found. Run: make tools"; exit 1; }
	cargo audit

scan-fs: require-docker ## Trivy: dependency CVEs, hardcoded secrets, IaC misconfig
	$(TRIVY) -v "$(CURDIR)":/src $(TRIVY_IGNORE_VOL) $(TRIVY_IMAGE) fs --no-progress \
		--scanners vuln,secret,misconfig \
		--severity $(SEVERITY) --exit-code 1 \
		--skip-dirs /src/target \
		$(TRIVY_IGNORE_ARG) /src

scan-secrets: require-docker ## Gitleaks: credentials in git history and the working tree
	docker run --rm -v "$(CURDIR)":/repo $(GITLEAKS_GIT) \
		$(GITLEAKS_IMAGE) git /repo --redact --verbose
	docker run --rm -v "$(CURDIR)":/repo \
		$(GITLEAKS_IMAGE) dir /repo --redact --verbose

scan-private: ## Home paths and e-mail addresses in files that would be pushed
	@files=$$(git ls-files -co --exclude-standard | grep -v '^Makefile$$'); \
	hits=$$( { grep -nIE '$(PRIVATE_PATHS)' $$files; \
		grep -nIoE '$(PRIVATE_EMAILS)' $$files | grep -vE '$(PRIVATE_ALLOW)'; } ); \
	if [ -n "$$hits" ]; then \
		echo "❌ possible personal data:"; echo "$$hits"; exit 1; \
	else echo "✅ no home paths or e-mail addresses in pushable files"; fi

sbom: require-docker ## Write a CycloneDX SBOM of the crate graph
	@mkdir -p $(SEC_OUT)
	$(TRIVY) -v "$(CURDIR)":/src -v "$(CURDIR)/$(SEC_OUT)":/out $(TRIVY_IMAGE) fs --no-progress \
		--format cyclonedx --output /out/sbom.cdx.json --skip-dirs /src/target /src
	@echo "wrote $(SEC_OUT)/sbom.cdx.json"

security: audit scan-fs scan-secrets scan-private ## All security checks (source only, fast)
	@echo "✅ security passed"

##@ 🧹 Clean

clean: ## Remove dist and security artifacts
	rm -rf $(DIST) $(SEC_OUT)

clean-all: ## Remove the whole target/ directory (asks for confirmation first)
	@echo "About to delete: target/"
	@printf "Proceed? [y/N] "; read ans; [ "$$ans" = "y" ] || { echo "aborted"; exit 1; }
	cargo clean

require-docker:
	@docker info >/dev/null 2>&1 || { \
		echo "❌ Docker is not running. Start Docker Desktop."; \
		echo "   Needed for: lint-workflows, scan-fs, scan-secrets, sbom"; \
		exit 1; }

# Every target, no exceptions: a directory sharing a target name would make
# make treat that target as already up to date.
.PHONY: help deps tools build dist install notices app icon signing-cert install-app test verify lint fmt format-check lint-workflows run \
	audit scan-fs scan-secrets scan-private sbom security clean clean-all require-docker
