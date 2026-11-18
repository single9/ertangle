# termade Makefile

CARGO       ?= $(shell PATH="$(HOME)/.cargo/bin:$(PATH)" which cargo 2>/dev/null || echo cargo)
# 自動偵測 pip 安裝的 ziglang；找不到時留空 (可用 ZIG_DIR=... 覆寫)
ZIG_DIR     ?= $(shell python3 -c 'import ziglang, os; print(os.path.dirname(ziglang.__file__))' 2>/dev/null)
BUILD_PATH  := $(if $(ZIG_DIR),$(ZIG_DIR):)$(HOME)/.cargo/bin:$(HOME)/.local/bin:$(PATH)

PREFIX      ?= $(HOME)/.local
BINDIR      ?= $(PREFIX)/bin
DIST_DIR    ?= dist

BIN_LINUX           := target/release/termade
BIN_MACOS_ARM64     := target/aarch64-apple-darwin/release/termade
BIN_MACOS_X86_64    := target/x86_64-apple-darwin/release/termade
BIN_MACOS_UNIVERSAL := target/universal2-apple-darwin/release/termade

.PHONY: check-zigbuild all build dev run tui daemon shutdown test clippy lint fmt \
        macos macos-arm64 macos-x86_64 macos-universal dist install install-completions clean help \
        i18n-extract i18n-check setup-hooks pre-commit \
        changelog changelog-unreleased release-bump

all: build

## 檢查 cargo-zigbuild 是否可用 (macOS 交叉編譯前置)
check-zigbuild:
	@PATH="$(BUILD_PATH)" $(CARGO) zigbuild --help >/dev/null 2>&1 || { \
	  echo "找不到 cargo-zigbuild。請先安裝："; \
	  echo "  cargo install cargo-zigbuild"; \
	  echo "  pip install ziglang   # 或自行安裝 zig 並設定 ZIG_DIR=<zig 所在目錄>"; \
	  exit 1; }

## 建置本機 (Linux/macOS) release 執行檔
build:
	PATH="$(BUILD_PATH)" $(CARGO) build --release

## 建置 debug 執行檔
dev:
	PATH="$(BUILD_PATH)" $(CARGO) build

## 啟動 TUI (若 daemon 未執行會自動背景啟動)
run tui: build
	./$(BIN_LINUX)

## 前景啟動 daemon
daemon: build
	./$(BIN_LINUX) daemon

## 關閉背景執行中的 daemon
shutdown:
	-./$(BIN_LINUX) shutdown

## 執行所有單元測試
test:
	PATH="$(BUILD_PATH)" $(CARGO) test

## 執行 Clippy 靜態檢查 (零警告)
clippy lint:
	PATH="$(BUILD_PATH)" $(CARGO) clippy --all-targets -- -D warnings

## 格式化程式碼
fmt:
	PATH="$(BUILD_PATH)" $(CARGO) fmt

## 從原始碼抽出 tr! 字串更新 locales/termade.pot 與各 .po (需要 xtr 與 gettext 的 msgmerge)
i18n-extract:
	PATH="$(BUILD_PATH)" sh scripts/i18n-extract.sh

## 檢查翻譯檔：.pot 同步、沒有 fuzzy 與缺譯、佔位符一致
i18n-check:
	PATH="$(BUILD_PATH)" sh scripts/i18n-check.sh

## 安裝 git pre-commit hook (自動檢查 fmt、clippy 與 i18n)
setup-hooks:
	PATH="$(BUILD_PATH)" sh scripts/setup-hooks.sh

## 一鍵執行 commit 前所有檢查與自動排版 (fmt、clippy 與 i18n)
pre-commit:
	@echo "==> 執行 commit 前檢查..."
	@echo "--> 自動格式化程式碼 (cargo fmt)..."
	@PATH="$(BUILD_PATH)" $(CARGO) fmt
	@echo "--> 檢查 Clippy 警告 (cargo clippy)..."
	@PATH="$(BUILD_PATH)" $(CARGO) clippy --all-targets -- -D warnings
	@echo "--> 檢查多國語系目錄 (i18n-check)..."
	@PATH="$(BUILD_PATH)" sh scripts/i18n-check.sh
	@echo "✅ 所有 commit 前檢查順利通過！"

## 使用 git-cliff 產生完整 CHANGELOG.md
changelog:
	PATH="$(BUILD_PATH)" git cliff -o CHANGELOG.md

## 預覽下一版 (unreleased) 變更紀錄
changelog-unreleased:
	PATH="$(BUILD_PATH)" git cliff --unreleased

## 準備發布新版本 (用法: make release-bump 或 make release-bump VERSION=0.4.0)
release-bump:
	@next_ver="$${VERSION:-$$(PATH="$(BUILD_PATH)" git cliff --bumped-version 2>/dev/null | tr -d 'v')}"; \
	if [ -z "$$next_ver" ]; then echo "無法自動計算版本號，請指定 VERSION=x.y.z" >&2; exit 1; fi; \
	echo "準備發布版本: v$$next_ver"; \
	sed -i.bak 's/^version = ".*"/version = "'$$next_ver'"/' Cargo.toml && rm -f Cargo.toml.bak; \
	PATH="$(BUILD_PATH)" $(CARGO) check >/dev/null 2>&1; \
	PATH="$(BUILD_PATH)" git cliff --tag "v$$next_ver" -o CHANGELOG.md; \
	echo "已更新 Cargo.toml 與 CHANGELOG.md 至 v$$next_ver"; \
	echo "請檢視變更後提交："; \
	echo "  git commit -am 'chore(release): prepare for v'$$next_ver"; \
	echo "  git tag -a v$$next_ver -m 'chore: release v'$$next_ver"

## 交叉編譯 macOS Apple Silicon (aarch64-apple-darwin)
macos-arm64: check-zigbuild
	PATH="$(BUILD_PATH)" $(CARGO) zigbuild --release --target aarch64-apple-darwin

## 交叉編譯 macOS Intel (x86_64-apple-darwin)
macos-x86_64: check-zigbuild
	PATH="$(BUILD_PATH)" $(CARGO) zigbuild --release --target x86_64-apple-darwin

## 交叉編譯 macOS Universal 2 (arm64 + x86_64 雙架構通用執行檔)
macos-universal: check-zigbuild
	PATH="$(BUILD_PATH)" $(CARGO) zigbuild --release --target universal2-apple-darwin

## 一次建置所有 macOS 版本 (arm64 / x86_64 / universal2)
macos: macos-arm64 macos-x86_64 macos-universal
	@ls -lh $(BIN_MACOS_ARM64) $(BIN_MACOS_X86_64) $(BIN_MACOS_UNIVERSAL)

## 打包 Linux 與 macOS 執行檔至 dist/ 目錄
dist: build macos
	@mkdir -p $(DIST_DIR)
	cp $(BIN_LINUX)           $(DIST_DIR)/termade-linux-x86_64
	cp $(BIN_MACOS_ARM64)     $(DIST_DIR)/termade-macos-arm64
	cp $(BIN_MACOS_X86_64)    $(DIST_DIR)/termade-macos-x86_64
	cp $(BIN_MACOS_UNIVERSAL) $(DIST_DIR)/termade-macos-universal
	@ls -lh $(DIST_DIR)/

## 安裝本機執行檔至 $(BINDIR)/termade (預設 ~/.local/bin/termade)
install: build
	@mkdir -p $(BINDIR)
	install -m 755 $(BIN_LINUX) $(BINDIR)/termade
	@echo "已安裝至 $(BINDIR)/termade"

## 依目前 SHELL 安裝 shell 補全腳本 (zsh / bash / fish)
install-completions: build
	@sh_name="$$(basename "$${SHELL:-}")"; \
	case "$$sh_name" in \
	  zsh)  dest="$(HOME)/.local/share/zsh/site-functions/_termade" ;; \
	  bash) dest="$(HOME)/.local/share/bash-completion/completions/termade" ;; \
	  fish) dest="$(HOME)/.config/fish/completions/termade.fish" ;; \
	  *)    echo "無法從 SHELL='$${SHELL:-}' 判斷 shell (請指定 SHELL=zsh|bash|fish)" >&2; exit 1 ;; \
	esac; \
	mkdir -p "$$(dirname "$$dest")"; \
	./$(BIN_LINUX) completions "$$sh_name" > "$$dest"; \
	echo "已安裝 $$sh_name 補全至 $$dest"

## 清除編譯產物與暫存 worktree
clean:
	PATH="$(BUILD_PATH)" $(CARGO) clean
	rm -rf $(DIST_DIR)

## 顯示可用目標說明
help:
	@echo "用法: make <target>"
	@echo ""
	@echo "常用目標:"
	@echo "  make build            建置本機 release 執行檔 (target/release/termade)"
	@echo "  make run              建置並啟動 TUI"
	@echo "  make test             執行單元測試"
	@echo "  make lint             執行 cargo clippy --all-targets -- -D warnings"
	@echo "  make install          安裝 termade 至 $(BINDIR)/termade"
	@echo "  make install-completions  依目前 SHELL 安裝補全腳本 (zsh / bash / fish)"
	@echo "  make i18n-extract     更新 locales/termade.pot 與各語言 .po"
	@echo "  make i18n-check       檢查翻譯檔 (CI 也會跑)"
	@echo "  make setup-hooks      安裝 git pre-commit hook (自動檢查 fmt、clippy 與 i18n)"
	@echo "  make pre-commit       一鍵執行 commit 前檢查 (自動 fmt、跑 clippy 與 i18n)"
	@echo ""
	@echo "版本與發布目標:"
	@echo "  make changelog        使用 git-cliff 產生完整 CHANGELOG.md"
	@echo "  make changelog-unreleased  預覽下一版 (unreleased) 變更紀錄"
	@echo "  make release-bump     自動計算或指定版本號更新 Cargo.toml 與 CHANGELOG.md"
	@echo ""
	@echo "macOS 交叉編譯目標:"
	@echo "  make macos            建置全部 macOS 版本 (arm64 / x86_64 / universal2)"
	@echo "  make macos-arm64      建置 target/aarch64-apple-darwin/release/termade"
	@echo "  make macos-x86_64     建置 target/x86_64-apple-darwin/release/termade"
	@echo "  make macos-universal  建置 target/universal2-apple-darwin/release/termade"
	@echo "  make dist             將 Linux 與 macOS 執行檔彙整至 dist/"
	@echo "  make clean            清除 target/ 與 dist/"
