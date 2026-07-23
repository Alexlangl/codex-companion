import { Component, type ErrorInfo, type ReactNode } from "react";
import { FolderOpen, RefreshCw, TriangleAlert } from "lucide-react";
import { Button } from "../components/ui";
import { openDiagnosticDirectory, reportFrontendError } from "../lib/api";

type AppErrorBoundaryProps = {
  children: ReactNode;
};

type AppErrorBoundaryState = {
  error: Error | null;
};

export class AppErrorBoundary extends Component<AppErrorBoundaryProps, AppErrorBoundaryState> {
  state: AppErrorBoundaryState = { error: null };

  static getDerivedStateFromError(error: Error): AppErrorBoundaryState {
    return { error };
  }

  componentDidMount(): void {
    window.addEventListener("error", this.handleWindowError);
    window.addEventListener("unhandledrejection", this.handleUnhandledRejection);
  }

  componentDidCatch(error: Error, info: ErrorInfo): void {
    void reportFrontendError(error.message, error.stack, info.componentStack);
  }

  componentWillUnmount(): void {
    window.removeEventListener("error", this.handleWindowError);
    window.removeEventListener("unhandledrejection", this.handleUnhandledRejection);
  }

  private handleWindowError = (event: ErrorEvent): void => {
    const stack = event.error instanceof Error ? event.error.stack : null;
    void reportFrontendError(event.message || "Unknown window error", stack);
  };

  private handleUnhandledRejection = (event: PromiseRejectionEvent): void => {
    const reason = normalizeError(event.reason);
    void reportFrontendError(`Unhandled promise rejection: ${reason.message}`, reason.stack);
  };

  private handleReload = (): void => {
    window.location.reload();
  };

  private handleOpenLogs = (): void => {
    void openDiagnosticDirectory();
  };

  render(): ReactNode {
    const { error } = this.state;
    if (!error) {
      return this.props.children;
    }

    return (
      <main className="fatal-error-shell">
        <section className="fatal-error-panel" aria-live="assertive">
          <TriangleAlert aria-hidden="true" size={24} />
          <div>
            <h1>界面遇到错误</h1>
            <p>错误已经写入 Companion 诊断日志。重新加载不会删除账号、分组或会话数据。</p>
            <code>{error.message}</code>
          </div>
          <div className="actions">
            <Button onClick={this.handleReload}>
              <RefreshCw aria-hidden="true" size={15} /> 重新加载界面
            </Button>
            <Button onClick={this.handleOpenLogs} variant="secondary">
              <FolderOpen aria-hidden="true" size={15} /> 打开诊断日志
            </Button>
          </div>
        </section>
      </main>
    );
  }
}

function normalizeError(reason: unknown): Error {
  if (reason instanceof Error) {
    return reason;
  }
  if (typeof reason === "string") {
    return new Error(reason);
  }
  try {
    return new Error(JSON.stringify(reason));
  } catch {
    return new Error(String(reason));
  }
}
