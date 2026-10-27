// Compatibility adapter for saved layouts. New navigation opens the product's Generate section.
import { codicon, ReactWidget } from "@theia/core/lib/browser";
import { inject, injectable, postConstruct } from "@theia/core/shared/inversify";
import React from "@theia/core/shared/react";
import { GenerateContent, GenerateViewServices } from "./generate-content";

@injectable()
export class GenerateWidget extends ReactWidget {
  static readonly ID = "gearbox.generate";
  static readonly LABEL = "Gearbox Generate";
  @inject(GenerateViewServices) protected readonly services!: GenerateViewServices;
  @postConstruct()
  protected init(): void {
    this.id = GenerateWidget.ID;
    this.title.label = GenerateWidget.LABEL;
    this.title.iconClass = codicon("checklist");
    this.title.closable = true;
    this.addClass("gbx-widget-generate");
    this.update();
  }
  protected render(): React.ReactNode {
    return <GenerateContent services={this.services} />;
  }
}
