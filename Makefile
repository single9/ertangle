# termade Makefile

CARGO       ?= $(shell PATH="$(HOME)/.cargo/bin:$(PATH)" which cargo 2>/dev/null || echo cargo)
# 自動偵測 pip 安裝的 ziglang；找不到時留空（可用 ZIG_DIR=... 覆寫）
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
        macos macos-arm64 macos-x86_64 macos-universal dist install clean help

all: build

## 檢查 cargo-zigbuild 是否可用（macOS 交叉編譯前置）
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

## 啟動 TUI（若 daemon 未執行會自動背景啟動）
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

## 執行 Clippy 靜態檢查（零警告）
clippy lint:
	PATH="$(BUILD_PATH)" $(CARGO) clippy --all-targets -- -D warnings

## 格式化程式碼
fmt:
	PATH="$(BUILD_PATH)" $(CARGO) fmt

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

## 安裝本機執行檔至 $(BINDIR)/termade（預設 ~/.local/bin/termade）
install: build
	@mkdir -p $(BINDIR)
	install -m 755 $(BIN_LINUX) $(BINDIR)/termade
	@echo "已安裝至 $(BINDIR)/termade"

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
	@echo ""
	@echo "macOS 交叉編譯目標:"
	@echo "  make macos            建置全部 macOS 版本 (arm64 / x86_64 / universal2)"
	@echo "  make macos-arm64      建置 target/aarch64-apple-darwin/release/termade"
	@echo "  make macos-x86_64     建置 target/x86_64-apple-darwin/release/termade"
	@echo "  make macos-universal  建置 target/universal2-apple-darwin/release/termade"
	@echo "  make dist             將 Linux 與 macOS 執行檔彙整至 dist/"
	@echo "  make clean            清除 target/ 與 dist/"
