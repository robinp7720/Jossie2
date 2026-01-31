use jossie_core::types::{Message, Role};
use uuid::Uuid;
use chrono::Utc;
use crate::state::AppState;

async fn build_system_prompt(state: &AppState) -> String {
    let mut prompt = state.system_prompt.clone();

    // Dynamically append agent and user profiles from memory
    if let Ok(Some(entry)) = state.db.get_memory("agent_profile").await {
        prompt.push_str("\n\n## Agent Description (Jossie)\n");
        prompt.push_str(&entry.content);
    }
    
    if let Ok(Some(entry)) = state.db.get_memory("user_profile").await {
        prompt.push_str("\n\n## User Description\n");
        prompt.push_str(&entry.content);
    }

    prompt
}

pub async fn prepend_system_prompt(state: &AppState, messages: &mut Vec<Message>) {
    let content = build_system_prompt(state).await;
    if content.is_empty() {
        return;
    }
    let sys_msg = Message {
        id: Uuid::nil(),
        conversation_id: Uuid::nil(),
        role: Role::System,
        content,
        tool_calls: None,
        tool_call_id: None,
        name: None,
        created_at: Utc::now(),
    };
    messages.insert(0, sys_msg);
}

pub async fn run_agent_loop(state: &AppState, conv_id: Uuid) -> anyhow::Result<String> {
    let tools = state.registry.all_tool_definitions();
    let mut messages = state.db.get_messages(conv_id).await?;
    prepend_system_prompt(state, &mut messages).await;

    for _iteration in 0..state.max_agent_iterations {
        let (content, tool_calls) = state.llm.complete(&messages, &tools).await?;

        if tool_calls.is_empty() {
            let msg = Message {
                id: Uuid::new_v4(),
                conversation_id: conv_id,
                role: Role::Assistant,
                content: content.clone(),
                tool_calls: None,
                tool_call_id: None,
                name: None,
                created_at: Utc::now(),
            };
            state.db.save_message(&msg).await?;
            return Ok(content);
        }

        let tc_json = serde_json::to_value(&tool_calls)?;
        let assistant_msg = Message {
            id: Uuid::new_v4(),
            conversation_id: conv_id,
            role: Role::Assistant,
            content: content.clone(),
            tool_calls: Some(tc_json),
            tool_call_id: None,
            name: None,
            created_at: Utc::now(),
        };
        state.db.save_message(&assistant_msg).await?;
        messages.push(assistant_msg);

        for call in &tool_calls {
            let result = state.registry.execute(call).await;
            let tool_msg = Message {
                id: Uuid::new_v4(),
                conversation_id: conv_id,
                role: Role::Tool,
                content: result.content,
                tool_calls: None,
                tool_call_id: Some(call.id.clone()),
                name: Some(call.name.clone()),
                created_at: Utc::now(),
            };
            state.db.save_message(&tool_msg).await?;
            messages.push(tool_msg);
        }
    }

    anyhow::bail!("Agent loop exceeded maximum of {} iterations", state.max_agent_iterations)
}
