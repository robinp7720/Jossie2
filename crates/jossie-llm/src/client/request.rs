impl LlmClient {
    fn build_input(
        &self,
        messages: &[Message],
        cache_breakpoint_message_index: Option<usize>,
    ) -> Vec<ResponseInputItem> {
        let mut items = Vec::new();
        let selected_attachments =
            select_attachment_ids(messages, self.max_attachment_bytes_per_request);

        for (message_index, message) in messages.iter().enumerate() {
            if message.role == Role::Assistant
                && let Some(response_items) = &message.response_items
                && !response_items.is_empty()
            {
                items.extend(response_items.iter().cloned().map(ResponseInputItem::Raw));
                continue;
            }

            match message.role {
                Role::System | Role::User => {
                    let mut content = vec![ResponseInputContent::Text(ResponseInputText {
                        item_type: "input_text",
                        text: message.content.clone(),
                        prompt_cache_breakpoint: (cache_breakpoint_message_index
                            == Some(message_index))
                        .then_some(PromptCacheBreakpoint { mode: "explicit" }),
                    })];
                    if message.role == Role::User
                        && let Some(attachments) = &message.attachments
                    {
                        for attachment in attachments {
                            if !selected_attachments.contains(&attachment.id) {
                                continue;
                            }
                            if let Some(input) = attachment_input(attachment) {
                                content.push(input);
                            }
                        }
                    }
                    items.push(ResponseInputItem::InputMessage(ResponseInputMessage {
                        item_type: "message",
                        role: message.role.to_string(),
                        content,
                    }));
                }
                Role::Assistant => {
                    if !message.content.is_empty() {
                        items.push(ResponseInputItem::AssistantMessage(
                            ResponseAssistantMessage {
                                item_type: "message",
                                role: "assistant",
                                content: vec![ResponseOutputText {
                                    item_type: "output_text",
                                    text: message.content.clone(),
                                }],
                            },
                        ));
                    }

                    if let Some(tool_calls) = tool_calls_from_message(message) {
                        for call in tool_calls {
                            items.push(ResponseInputItem::FunctionCall(
                                ResponseFunctionCallInput {
                                    item_type: "function_call",
                                    call_id: call.id,
                                    name: call.name,
                                    arguments: call.arguments,
                                },
                            ));
                        }
                    }
                }
                Role::Tool => {
                    if let Some(call_id) = &message.tool_call_id {
                        items.push(ResponseInputItem::FunctionCallOutput(
                            ResponseFunctionCallOutputInput {
                                item_type: "function_call_output",
                                call_id: call_id.clone(),
                                output: message.content.clone(),
                            },
                        ));
                    }
                }
            }
        }

        items
    }

    fn build_tools(&self, tools: &[ToolDefinition]) -> Option<Vec<ResponseTool>> {
        let mut built: Vec<ResponseTool> = tools
            .iter()
            .map(|tool| {
                ResponseTool::Function(ResponseFunctionTool {
                    item_type: "function",
                    name: tool.name.clone(),
                    description: tool.description.clone(),
                    parameters: tool.parameters.clone(),
                    strict: None,
                })
            })
            .collect();

        if self.enable_web_search {
            built.push(ResponseTool::Hosted(ResponseHostedTool {
                item_type: "web_search",
            }));
        }

        if built.is_empty() { None } else { Some(built) }
    }

    fn build_request(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
        stream: bool,
        options: &LlmRequestOptions,
    ) -> ResponsesRequest {
        let built_tools = self.build_tools(tools);
        let has_tools = built_tools.is_some();

        ResponsesRequest {
            model: self.model.clone(),
            input: self.build_input(messages, options.cache_breakpoint_message_index),
            tools: built_tools,
            tool_choice: if has_tools {
                Some(Value::String("auto".to_string()))
            } else {
                None
            },
            stream,
            reasoning: if self.reasoning_effort.is_some() || self.reasoning_context.is_some() {
                Some(ResponseReasoningConfig {
                    effort: self.reasoning_effort.clone(),
                    context: self.reasoning_context.clone(),
                })
            } else {
                None
            },
            service_tier: self.service_tier.clone(),
            previous_response_id: options.previous_response_id.clone(),
            prompt_cache_key: options.prompt_cache_key.clone(),
            prompt_cache_options: options
                .cache_breakpoint_message_index
                .map(|_| PromptCacheOptions { mode: "explicit" }),
            store: options.previous_response_id.as_ref().map(|_| true),
            text: options
                .structured_output
                .as_ref()
                .map(|format| ResponseTextConfig {
                    format: ResponseTextFormat {
                        format_type: "json_schema",
                        name: format.name.clone(),
                        strict: true,
                        schema: format.schema.clone(),
                    },
                }),
        }
    }

}
