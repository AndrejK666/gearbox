// A plugin host is never started for a window that has already gone.
//
// `HostedPluginServerImpl.getInstalledPluginIds` is an RPC from the frontend
// that waits for the deployer and only then calls `runPluginServer`. A window
// closed while it waits has already had `clientClosed` -- which found no child
// to terminate -- so the fork that follows belongs to nobody: nothing will ever
// terminate it, and it spends the rest of the server's life calling
// `NotificationMain` and `LoggerMain` on a connection that no longer exists,
// logging each failure through the same dead connection. In a conformance run
// that was ~97,000 errors over forty minutes from one window closed early, and
// the two claims that ran under that load timed out.
//
// Bound per connection, after `@theia/plugin-ext`'s own connection module.

import { injectable } from "@theia/core/shared/inversify";
import type { HostedPluginClient } from "@theia/plugin-ext/lib/common/plugin-protocol";
import { HostedPluginSupport } from "@theia/plugin-ext/lib/hosted/node/hosted-plugin";

@injectable()
export class ClosedWindowHostedPluginSupport extends HostedPluginSupport {
  private closed = false;

  override setClient(client: HostedPluginClient): void {
    // A reconnecting window is a client again.
    this.closed = false;
    super.setClient(client);
  }

  override clientClosed(): void {
    this.closed = true;
    super.clientClosed();
  }

  override runPluginServer(serverName?: string): void {
    if (this.closed) {
      return;
    }
    super.runPluginServer(serverName);
  }
}
