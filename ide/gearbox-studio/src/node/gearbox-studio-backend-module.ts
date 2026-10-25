// One service per frontend connection, one engine per service.

import {
  ConnectionHandler,
  RpcConnectionHandler,
} from "@theia/core/lib/common/messaging";
import { ContainerModule } from "@theia/core/shared/inversify";

import { GEARBOX_SERVICE_PATH, GearboxClient } from "../common/protocol";
import { GearboxServiceImpl } from "./gearbox-service-impl";
import { LanguageModelProvider } from "@theia/ai-core";
import { ConnectionContainerModule } from "@theia/core/lib/node/messaging/connection-container-module";
import { HostedPluginSupport } from "@theia/plugin-ext/lib/hosted/node/hosted-plugin";
import { claudeCodeModels } from "./claude-code-model";
import { ClosedWindowHostedPluginSupport } from "./plugin-host-guard";

export default new ContainerModule((bind) => {
  bind(ConnectionContainerModule).toConstantValue(ConnectionContainerModule.create(({bind, isBound, rebind}) => {
    bind(LanguageModelProvider).toConstantValue(claudeCodeModels);
    // See `plugin-host-guard.ts`. This module loads after plugin-ext's, so its
    // binding is the one replaced.
    if (isBound(HostedPluginSupport)) {
      rebind(HostedPluginSupport).to(ClosedWindowHostedPluginSupport).inSingletonScope();
    }
  }));
  bind(ConnectionHandler)
    .toDynamicValue(
      ({ container }) =>
        new RpcConnectionHandler<GearboxClient>(
          GEARBOX_SERVICE_PATH,
          (client) => {
            // A child container per connection, so each frontend gets its own
            // service and its own engine.
            //
            // The service used to be a process-wide singleton with the client
            // swapped in on every connection, which is the worst of both worlds:
            // a second window's `setClient` stopped the first window receiving
            // projections, and its `initialize()` disposed the engine the first
            // window was still loading from. One engine shared by several clients
            // is a coherent design too, but it needs a set of clients and a
            // refcounted engine -- not one slot written by whoever connected last.
            const child = container.createChild();
            child.bind(GearboxServiceImpl).toSelf().inSingletonScope();
            const service = child.get<GearboxServiceImpl>(GearboxServiceImpl);
            service.setClient(client);
            // Theia's handler never disposes the target, so without this a closed
            // window leaves its engine running and holding the source root open.
            client.onDidCloseConnection(() => service.dispose());
            return service;
          },
        ),
    )
    .inSingletonScope();
});
