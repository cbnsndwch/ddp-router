const { MongoClient } = require('mongodb');

async function killAllChangeStreams() {
  const uri = 'mongodb://localhost:3001/meteor';
  const client = new MongoClient(uri);

  try {
    await client.connect();
    // currentOp and killOp commands must be executed against the admin database
    const adminDb = client.db('admin');

    // Fetch all current operations
    const { inprog } = await adminDb.command({ currentOp: 1 });

    // Filter operations to find those running a $changeStream aggregation
    const changeStreamOps = inprog.filter(op => 
      op.command && 
      op.command.aggregate && 
      op.command.pipeline && 
      op.command.pipeline.some(stage => stage.$changeStream)
    );

    if (changeStreamOps.length === 0) {
      console.log('No active change streams found.');
    } else {
      console.log(`Found ${changeStreamOps.length} active change stream(s). Terminating...`);
      
      for (const op of changeStreamOps) {
        console.log(`Killing operation ID: ${op.opid}`);
        await adminDb.command({ killOp: 1, op: op.opid });
      }
      
      console.log('All change streams have been terminated.');
    }
  } catch (error) {
    console.error('Error terminating change streams:', error);
  } finally {
    await client.close();
  }
}

killAllChangeStreams();